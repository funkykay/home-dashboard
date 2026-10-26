use crate::{
    display::Display,
    model::{Align, Color, HEIGHT, Rect, WIDTH},
};
use anyhow::Result;
use fontdue::{Font, FontSettings, Metrics};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Draw {
    Fill(Rect, Color),
    Line {
        from: (u32, u32),
        to: (u32, u32),
        color: Color,
    },
    Circle {
        x: u32,
        y: u32,
        radius: u32,
        color: Color,
    },
    Text {
        text: String,
        x: u32,
        y: u32,
        width: u32,
        size: f32,
        color: Color,
        align: Align,
    },
}
#[derive(Clone, Debug)]
pub struct RenderItem {
    pub id: String,
    pub rect: Rect,
    pub generation: u64,
    pub revision: u64,
    pub animate: bool,
    pub draws: Vec<Draw>,
}
#[derive(Default)]
pub struct Dirty {
    pub regions: Vec<Rect>,
}
impl Dirty {
    pub fn add(&mut self, rect: Rect) {
        let Some(mut rect) = rect.intersection(Rect::SCREEN) else {
            return;
        };
        let mut i = 0;
        while i < self.regions.len() {
            if rect.intersection(self.regions[i]).is_some() {
                rect = rect.union(self.regions.swap_remove(i));
                i = 0;
            } else {
                i += 1;
            }
        }
        self.regions.push(rect);
        if self.regions.len() > 64 {
            self.regions.clear();
            self.regions.push(Rect::SCREEN);
        }
    }
}
struct Glyph {
    metrics: Metrics,
    pixels: Vec<u8>,
    used: u64,
}
pub struct Renderer {
    pub pixels: Vec<u32>,
    font: Font,
    cache: HashMap<(char, u16), Glyph>,
    cache_bytes: usize,
    clock: u64,
}
impl Renderer {
    pub fn new(font_bytes: Vec<u8>) -> Result<Self> {
        Ok(Self {
            pixels: vec![0; (WIDTH * HEIGHT) as usize],
            font: Font::from_bytes(font_bytes, FontSettings::default())
                .map_err(anyhow::Error::msg)?,
            cache: HashMap::new(),
            cache_bytes: 0,
            clock: 0,
        })
    }
    pub fn paint(&mut self, items: &[RenderItem], regions: &[Rect], phase: u32) {
        for &region in regions {
            for y in region.y..region.bottom() {
                self.pixels[(y * WIDTH + region.x) as usize..(y * WIDTH + region.right()) as usize]
                    .fill(0x080F1C);
            }
            if items.is_empty() {
                self.draw(
                    &Draw::Text {
                        text: "Wie sie sehen, sehen Sie nichts.".into(),
                        x: 0,
                        y: 510,
                        width: WIDTH,
                        size: 40.0,
                        color: Color(230, 235, 245, 255),
                        align: Align::Center,
                    },
                    region,
                );
            }
            for item in items {
                if let Some(clip) = region.intersection(item.rect) {
                    for draw in &item.draws {
                        self.draw(draw, clip);
                    }
                    if item.animate {
                        let radius = 5;
                        let x = item.rect.x + 12 + (phase % 30) * 2;
                        self.draw(
                            &Draw::Circle {
                                x,
                                y: item.rect.bottom().saturating_sub(12),
                                radius,
                                color: Color(56, 189, 248, 255),
                            },
                            clip,
                        );
                    }
                }
            }
        }
    }
    fn draw(&mut self, draw: &Draw, clip: Rect) {
        match draw {
            Draw::Fill(rect, color) => {
                if let Some(r) = clip.intersection(*rect) {
                    for y in r.y..r.bottom() {
                        for x in r.x..r.right() {
                            blend(&mut self.pixels, x, y, *color, 255);
                        }
                    }
                }
            }
            Draw::Line { from, to, color } => {
                let (mut x, mut y) = (from.0 as i32, from.1 as i32);
                let (tx, ty) = (to.0 as i32, to.1 as i32);
                let dx = (tx - x).abs();
                let sx = if x < tx { 1 } else { -1 };
                let dy = -(ty - y).abs();
                let sy = if y < ty { 1 } else { -1 };
                let mut err = dx + dy;
                loop {
                    if inside(clip, x, y) {
                        blend(&mut self.pixels, x as u32, y as u32, *color, 255);
                    }
                    if x == tx && y == ty {
                        break;
                    }
                    let e = 2 * err;
                    if e >= dy {
                        err += dy;
                        x += sx;
                    }
                    if e <= dx {
                        err += dx;
                        y += sy;
                    }
                }
            }
            Draw::Circle {
                x,
                y,
                radius,
                color,
            } => {
                let radius = (*radius).min(1920) as i32;
                for py in clip.y..clip.bottom() {
                    let dy = py as i32 - *y as i32;
                    if dy.abs() > radius {
                        continue;
                    }
                    for px in clip.x..clip.right() {
                        let dx = px as i32 - *x as i32;
                        if dx * dx + dy * dy <= radius * radius {
                            blend(&mut self.pixels, px, py, *color, 255);
                        }
                    }
                }
            }
            Draw::Text {
                text,
                x,
                y,
                width,
                size,
                color,
                align,
            } => {
                let Some(clip) = clip.intersection(Rect {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: HEIGHT.saturating_sub(*y),
                }) else {
                    return;
                };
                let size = (*size as u16).clamp(8, 128);
                let mut extent = 0.0;
                let mut previous = None;
                for ch in text.chars() {
                    if let Some(prev) = previous {
                        extent += self
                            .font
                            .horizontal_kern(prev, ch, f32::from(size))
                            .unwrap_or(0.0);
                    }
                    extent += self.font.metrics(ch, f32::from(size)).advance_width;
                    previous = Some(ch);
                }
                let mut pen = *x as f32
                    + match align {
                        Align::Left => 0.0,
                        Align::Center => (*width as f32 - extent).max(0.0) / 2.0,
                        Align::Right => (*width as f32 - extent).max(0.0),
                    };
                let baseline = *y as i32
                    + self
                        .font
                        .horizontal_line_metrics(f32::from(size))
                        .map(|m| m.ascent.ceil() as i32)
                        .unwrap_or(size as i32);
                previous = None;
                for ch in text.chars() {
                    if pen >= clip.right() as f32 {
                        break;
                    }
                    if let Some(prev) = previous {
                        pen += self
                            .font
                            .horizontal_kern(prev, ch, f32::from(size))
                            .unwrap_or(0.0);
                    }
                    previous = Some(ch);
                    self.clock += 1;
                    let key = (ch, size);
                    if !self.cache.contains_key(&key) {
                        let (metrics, pixels) = self.font.rasterize(ch, f32::from(size));
                        while self.cache_bytes + pixels.len() > 4 * 1024 * 1024
                            || self.cache.len() >= 2048
                        {
                            if let Some(key) = self
                                .cache
                                .iter()
                                .min_by_key(|(_, g)| g.used)
                                .map(|(k, _)| *k)
                            {
                                let old = self.cache.remove(&key).unwrap();
                                self.cache_bytes -= old.pixels.len();
                            } else {
                                break;
                            }
                        }
                        self.cache_bytes += pixels.len();
                        self.cache.insert(
                            key,
                            Glyph {
                                metrics,
                                pixels,
                                used: self.clock,
                            },
                        );
                    }
                    let glyph = self.cache.get_mut(&key).unwrap();
                    glyph.used = self.clock;
                    let m = glyph.metrics;
                    let left = pen.round() as i32 + m.xmin;
                    let top = baseline - m.ymin - m.height as i32;
                    for gy in 0..m.height {
                        let py = top + gy as i32;
                        if py < clip.y as i32 || py >= clip.bottom() as i32 {
                            continue;
                        }
                        for gx in 0..m.width {
                            let px = left + gx as i32;
                            if inside(clip, px, py) {
                                blend(
                                    &mut self.pixels,
                                    px as u32,
                                    py as u32,
                                    *color,
                                    glyph.pixels[gy * m.width + gx],
                                );
                            }
                        }
                    }
                    pen += m.advance_width;
                }
            }
        }
    }
}
fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x as i32 && x < r.right() as i32 && y >= r.y as i32 && y < r.bottom() as i32
}
fn blend(pixels: &mut [u32], x: u32, y: u32, c: Color, coverage: u8) {
    let a = u32::from(c.3) * u32::from(coverage) / 255;
    if a == 0 {
        return;
    }
    let p = &mut pixels[(y * WIDTH + x) as usize];
    let inv = 255 - a;
    let r = (u32::from(c.0) * a + ((*p >> 16) & 255) * inv + 127) / 255;
    let g = (u32::from(c.1) * a + ((*p >> 8) & 255) * inv + 127) / 255;
    let b = (u32::from(c.2) * a + (*p & 255) * inv + 127) / 255;
    *p = (r << 16) | (g << 8) | b;
}
#[derive(Default)]
pub struct RenderStats {
    pub frames: AtomicU64,
    pub pixels: AtomicU64,
    pub error: Mutex<Option<String>>,
}
#[derive(Clone)]
pub struct RenderHandle {
    latest: Arc<Mutex<Option<Vec<RenderItem>>>>,
    wake: crossbeam_channel::Sender<()>,
    pub stats: Arc<RenderStats>,
}
impl RenderHandle {
    pub fn submit(&self, items: Vec<RenderItem>) {
        *self.latest.lock().unwrap() = Some(items);
        let _ = self.wake.try_send(());
    }
}
pub struct RenderThread {
    pub handle: RenderHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl RenderThread {
    pub fn spawn(mut display: Box<dyn Display>, mut renderer: Renderer) -> Result<Self> {
        let (wake, rx) = crossbeam_channel::bounded(1);
        let latest = Arc::new(Mutex::new(Some(Vec::new())));
        let stats = Arc::new(RenderStats::default());
        let handle = RenderHandle {
            latest: latest.clone(),
            wake,
            stats: stats.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("dashboard-render".into())
            .spawn(move || {
                let mut current: Vec<RenderItem> = Vec::new();
                let mut first = true;
                let animation_start = Instant::now();
                let mut last = Instant::now() - Duration::from_secs(1);
                loop {
                    let next = latest.lock().unwrap().take();
                    let mut dirty = Dirty::default();
                    if first {
                        dirty.add(Rect::SCREEN);
                        first = false;
                    }
                    if let Some(next) = next {
                        let reordered = current.iter().map(|v| &v.id).collect::<Vec<_>>()
                            != next.iter().map(|v| &v.id).collect::<Vec<_>>();
                        if reordered {
                            for c in current.iter().chain(next.iter()) {
                                dirty.add(c.rect);
                            }
                            if current.is_empty() || next.is_empty() {
                                dirty.add(Rect::SCREEN);
                            }
                        }
                        for old in &current {
                            if !next.iter().any(|n| {
                                n.id == old.id
                                    && n.generation == old.generation
                                    && n.revision == old.revision
                                    && n.rect == old.rect
                            }) {
                                dirty.add(old.rect);
                            }
                        }
                        for new in &next {
                            if !current.iter().any(|o| {
                                o.id == new.id
                                    && o.generation == new.generation
                                    && o.revision == new.revision
                                    && o.rect == new.rect
                            }) {
                                dirty.add(new.rect);
                            }
                        }
                        current = next;
                    }
                    let animated = current.iter().any(|c| c.animate);
                    if animated {
                        for c in &current {
                            if c.animate
                                && let Some(region) = c.rect.intersection(Rect {
                                    x: c.rect.x + 6,
                                    y: c.rect.bottom().saturating_sub(18),
                                    width: 70,
                                    height: 12,
                                })
                            {
                                dirty.add(region);
                            }
                        }
                    }
                    if !dirty.regions.is_empty() {
                        // One presentation per 34 ms, coalescing queued updates during load.
                        let wait = Duration::from_millis(34).saturating_sub(last.elapsed());
                        if !wait.is_zero() {
                            std::thread::sleep(wait);
                        }
                        renderer.paint(
                            &current,
                            &dirty.regions,
                            (animation_start.elapsed().as_millis() / 34) as u32,
                        );
                        if let Err(e) = display.present(&renderer.pixels, &dirty.regions) {
                            *stats.error.lock().unwrap() = Some(e.to_string());
                            tracing::error!(error=%e,"display output failed");
                            break;
                        }
                        stats.frames.fetch_add(1, Ordering::Relaxed);
                        stats.pixels.fetch_add(
                            dirty
                                .regions
                                .iter()
                                .map(|r| u64::from(r.width) * u64::from(r.height))
                                .sum(),
                            Ordering::Relaxed,
                        );
                        last = Instant::now();
                    }
                    if animated {
                        if let Err(crossbeam_channel::RecvTimeoutError::Disconnected) =
                            rx.recv_timeout(Duration::from_millis(34))
                        {
                            break;
                        }
                    } else if rx.recv().is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
    pub fn shutdown(mut self) {
        drop(self.handle);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
pub fn load_font(path: Option<&str>) -> Result<Vec<u8>> {
    if let Some(path) = path {
        return Ok(std::fs::read(path)?);
    }
    for path in [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/Library/Fonts/Arial.ttf",
    ] {
        if let Ok(data) = std::fs::read(path) {
            return Ok(data);
        }
    }
    anyhow::bail!("no TTF/OTF font found; set DASHBOARD_FONT")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dirty_merge_transitive() {
        let mut d = Dirty::default();
        d.add(Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        });
        d.add(Rect {
            x: 15,
            y: 0,
            width: 10,
            height: 10,
        });
        d.add(Rect {
            x: 5,
            y: 0,
            width: 20,
            height: 10,
        });
        assert_eq!(
            d.regions,
            vec![Rect {
                x: 0,
                y: 0,
                width: 25,
                height: 10
            }]
        );
    }
    #[test]
    fn transparent_blend() {
        let mut p = vec![0; (WIDTH * HEIGHT) as usize];
        blend(&mut p, 0, 0, Color(255, 0, 0, 128), 255);
        assert_eq!(p[0], 0x800000);
    }
}
