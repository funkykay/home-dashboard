use crate::{
    display::Display,
    model::{Align, Color, HEIGHT, Rect, WIDTH},
};
use anyhow::Result;
use fontdue::{Font, FontSettings, Metrics};
use std::{
    collections::{HashMap, VecDeque},
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
            let other = self.regions[i];
            // A small per-region allowance pays for traversal/presentation overhead.
            // Keep sparse crosses and diagonal overlaps separate: paint resets each
            // clip before compositing, so overlapping clips remain alpha-correct.
            let area = |r: Rect| u64::from(r.width) * u64::from(r.height);
            if rect.intersection(other).is_some()
                && area(rect.union(other)) <= area(rect) + area(other) + 128
            {
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
    recent: bool,
}
pub struct Renderer {
    pub pixels: Vec<u32>,
    font: Font,
    cache: HashMap<(char, u16), Glyph>,
    cache_bytes: usize,
    eviction: VecDeque<(char, u16)>,
}
impl Renderer {
    pub fn new(font_bytes: Vec<u8>) -> Result<Self> {
        Ok(Self {
            pixels: vec![0; (WIDTH * HEIGHT) as usize],
            font: Font::from_bytes(font_bytes, FontSettings::default())
                .map_err(anyhow::Error::msg)?,
            cache: HashMap::new(),
            cache_bytes: 0,
            eviction: VecDeque::new(),
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
                    let key = (ch, size);
                    if !self.cache.contains_key(&key) {
                        let (metrics, pixels) = self.font.rasterize(ch, f32::from(size));
                        while self.cache_bytes + pixels.len() > 4 * 1024 * 1024
                            || self.cache.len() >= 2048
                        {
                            // Bounded second-chance clock: hits only set one bit, and
                            // each pass clears it. No full HashMap minimum search per miss.
                            let Some(old_key) = self.eviction.pop_front() else {
                                break;
                            };
                            let old = self.cache.get_mut(&old_key).unwrap();
                            if old.recent {
                                old.recent = false;
                                self.eviction.push_back(old_key);
                            } else {
                                let old = self.cache.remove(&old_key).unwrap();
                                self.cache_bytes -= old.pixels.len();
                            }
                        }
                        self.eviction.push_back(key);
                        self.cache_bytes += pixels.len();
                        self.cache.insert(
                            key,
                            Glyph {
                                metrics,
                                pixels,
                                recent: true,
                            },
                        );
                    }
                    let glyph = self.cache.get_mut(&key).unwrap();
                    glyph.recent = true;
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
    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
    #[test]
    fn sparse_cross_and_diagonal_updates_stay_small_and_bounded() {
        let mut dirty = Dirty::default();
        dirty.add(rect(0, 100, 1000, 10));
        dirty.add(rect(500, 0, 10, 1000));
        assert_eq!(dirty.regions.len(), 2);
        let area = |regions: &[Rect]| regions.iter().map(|r| r.width * r.height).sum::<u32>();
        assert_eq!(area(&dirty.regions), 20_000); // old merge: 1,000,000 pixels
        let mut diagonal = Dirty::default();
        for i in 0..20 {
            diagonal.add(rect(i * 40, i * 40, 50, 50));
        }
        assert_eq!(area(&diagonal.regions), 50_000); // old merge: 656,100 pixels
        for i in 0..100 {
            diagonal.add(rect(i * 10, 1000, 2, 2));
        }
        assert!(diagonal.regions.len() <= 64);
        assert_eq!(diagonal.regions, [Rect::SCREEN]);
    }
    #[test]
    fn overlapping_cost_based_regions_preserve_alpha_and_draw_order() {
        let font = load_font(None).unwrap();
        let mut partial = Renderer::new(font.clone()).unwrap();
        let mut full = Renderer::new(font).unwrap();
        let mut items = vec![
            RenderItem {
                id: "h".into(),
                rect: rect(0, 100, 1000, 10),
                generation: 1,
                revision: 0,
                animate: false,
                draws: vec![Draw::Fill(rect(0, 100, 1000, 10), Color(255, 0, 0, 128))],
            },
            RenderItem {
                id: "v".into(),
                rect: rect(500, 0, 10, 1000),
                generation: 1,
                revision: 0,
                animate: false,
                draws: vec![Draw::Fill(rect(500, 0, 10, 1000), Color(0, 255, 0, 128))],
            },
        ];
        partial.paint(&items, &[Rect::SCREEN], 0);
        items[0].draws = vec![Draw::Fill(items[0].rect, Color(0, 0, 255, 128))];
        items.reverse();
        let mut dirty = Dirty::default();
        for item in &items {
            dirty.add(item.rect);
        }
        partial.paint(&items, &dirty.regions, 0);
        full.paint(&items, &[Rect::SCREEN], 0);
        assert_eq!(partial.pixels, full.pixels);
    }
    fn text_draw(text: String, size: f32) -> Draw {
        Draw::Text {
            text,
            x: 0,
            y: 0,
            width: WIDTH,
            size,
            color: Color(255, 255, 255, 255),
            align: Align::Left,
        }
    }
    #[test]
    fn glyph_cache_reuses_text_and_stays_bounded_under_unicode_size_churn() {
        let mut renderer = Renderer::new(load_font(None).unwrap()).unwrap();
        let draw = text_draw("Wetter 12°C · Grüße".into(), 24.0);
        renderer.draw(&draw, Rect::SCREEN);
        let entries = renderer.cache.len();
        let bytes = renderer.cache_bytes;
        let address = renderer.cache[&('W', 24)].pixels.as_ptr();
        for _ in 0..100 {
            renderer.draw(&draw, Rect::SCREEN);
        }
        assert_eq!(renderer.cache.len(), entries);
        assert_eq!(renderer.cache_bytes, bytes);
        assert_eq!(renderer.cache[&('W', 24)].pixels.as_ptr(), address);
        // Includes supported Latin/Greek/Cyrillic and missing Unicode glyphs.
        for size in [8, 24, 64, 128] {
            for code in 32..1100 {
                let ch = char::from_u32(code).unwrap();
                renderer.draw(&text_draw(ch.to_string(), size as f32), rect(0, 0, 1, 1));
                assert!(renderer.cache.len() <= 2048);
                assert!(renderer.cache_bytes <= 4 * 1024 * 1024);
                assert_eq!(renderer.eviction.len(), renderer.cache.len());
            }
        }
        assert_eq!(
            renderer.cache_bytes,
            renderer
                .cache
                .values()
                .map(|g| g.pixels.len())
                .sum::<usize>()
        );
        let mut keys = renderer.eviction.iter().collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), renderer.cache.len());
        assert!(keys.iter().all(|key| renderer.cache.contains_key(key)));
    }
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
