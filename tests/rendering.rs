use pi_dashboard::{
    display::Headless,
    model::{Color, Rect, WIDTH},
    render::{Draw, RenderItem, RenderThread, Renderer, load_font},
};
use std::{sync::atomic::Ordering, time::Duration};
fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}
fn item(id: &str, r: Rect, c: Color) -> RenderItem {
    RenderItem {
        id: id.into(),
        rect: r,
        generation: 1,
        revision: 0,
        animate: false,
        draws: vec![Draw::Fill(r, c)],
    }
}
#[test]
fn partial_repaint_matches_full_composition_and_clips() {
    let font = load_font(None).expect("install DejaVu Sans / Arial for rendering tests");
    let mut a = Renderer::new(font.clone()).unwrap();
    let mut b = Renderer::new(font).unwrap();
    let mut items = vec![
        item("background", rect(0, 0, 100, 100), Color(220, 30, 0, 255)),
        item("overlay", rect(50, 20, 100, 100), Color(0, 200, 60, 128)),
    ];
    a.paint(&items, &[Rect::SCREEN], 0);
    let untouched = a.pixels[1000];
    items[0] = item("background", rect(0, 0, 100, 100), Color(30, 30, 220, 255));
    a.paint(&items, &[rect(0, 0, 100, 100)], 0);
    b.paint(&items, &[Rect::SCREEN], 0);
    assert_eq!(a.pixels, b.pixels);
    assert_eq!(a.pixels[1000], untouched);
    assert_ne!(
        a.pixels[(30 * WIDTH + 60) as usize],
        a.pixels[(30 * WIDTH + 10) as usize]
    );
    items.remove(1);
    a.paint(&items, &[rect(50, 20, 100, 100)], 0);
    b.paint(&items, &[Rect::SCREEN], 0);
    assert_eq!(a.pixels, b.pixels);
}
#[test]
fn idle_renderer_sleeps_and_animation_stops_after_removal() {
    let r = RenderThread::spawn(
        Box::new(Headless {
            presents: 0,
            output: None,
        }),
        Renderer::new(load_font(None).unwrap()).unwrap(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(180));
    let first = r.handle.stats.frames.load(Ordering::Relaxed);
    assert_eq!(first, 1);
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(r.handle.stats.frames.load(Ordering::Relaxed), first);
    let mut a = item("animated", rect(0, 0, 200, 200), Color(0, 0, 0, 255));
    a.animate = true;
    r.handle.submit(vec![a]);
    std::thread::sleep(Duration::from_millis(250));
    let frames = r.handle.stats.frames.load(Ordering::Relaxed);
    assert!((3..=9).contains(&(frames - first)));
    r.handle.submit(Vec::new());
    std::thread::sleep(Duration::from_millis(180));
    let settled = r.handle.stats.frames.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(r.handle.stats.frames.load(Ordering::Relaxed), settled);
    r.shutdown();
}
