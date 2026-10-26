use anyhow::Result;
use axum::{
    Router,
    body::{Body, Bytes},
    routing::get,
};
use pi_dashboard::{
    audio::output::{Output, OutputFactory},
    engine::{Engine, EngineHandle, Operation},
    model::{Command, Patch, Scene},
};
use serde_json::json;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct Counts {
    fail_next: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    opened: AtomicUsize,
    samples: AtomicUsize,
    connections: AtomicUsize,
}
struct Factory(Arc<Counts>);
struct Sink(Arc<Counts>);
impl OutputFactory for Factory {
    fn open(&self, _: u32, _: u32) -> Result<Box<dyn Output>> {
        if self.0.fail_next.swap(false, Ordering::SeqCst) {
            anyhow::bail!("simulated device-open error");
        }
        let n = self.0.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.peak.fetch_max(n, Ordering::SeqCst);
        self.0.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Sink(self.0.clone())))
    }
}
impl Output for Sink {
    fn write(&mut self, data: &[i16], _: &CancellationToken) -> Result<()> {
        assert!(!data.is_empty());
        self.0.samples.fetch_add(data.len(), Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
struct ConnectionGuard(Arc<Counts>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}
async fn stream(bytes: &'static [u8], counts: Arc<Counts>) -> Body {
    counts.connections.fetch_add(1, Ordering::SeqCst);
    let guard = ConnectionGuard(counts);
    let stream = futures_util::stream::unfold((0usize, guard), move |(offset, guard)| async move {
        tokio::time::sleep(Duration::from_millis(2)).await;
        let end = (offset + 512).min(bytes.len());
        let chunk = Bytes::from_static(&bytes[offset..end]);
        Some((
            Ok::<_, Infallible>(chunk),
            (if end == bytes.len() { 0 } else { end }, guard),
        ))
    });
    Body::from_stream(stream)
}
async fn wait_state(e: &EngineHandle, id: &str, state: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut previous = serde_json::Value::Null;
        loop {
            let s = e.request(Operation::Status(id.into())).await.unwrap();
            if previous != s {
                eprintln!("{id} waiting for {state}: {s}");
                previous = s.clone();
            }
            if s["state"] == state {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_mp3_aac_exclusive_output_and_network_cleanup() {
    for iteration in 0..20 {
        eprintln!("audio lifecycle iteration {iteration}");
        tokio::time::timeout(Duration::from_secs(10), pipeline_once())
            .await
            .expect("audio lifecycle timed out");
    }
}
async fn pipeline_once() {
    let counts = Arc::new(Counts::default());
    let c = counts.clone();
    let d = counts.clone();
    let app = Router::new()
        .route(
            "/mp3",
            get(move || stream(include_bytes!("fixtures/tone.mp3"), c.clone())),
        )
        .route(
            "/aac",
            get(move || stream(include_bytes!("fixtures/tone.aac"), d.clone())),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    let token = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(token.cancelled_owned())
            .await
            .unwrap();
    });
    let (e, t) = Engine::spawn(None, Arc::new(Factory(counts.clone())), "test".into());
    let _engine_on_drop = e.cancellation().drop_guard();
    let scene:Scene=serde_json::from_value(json!({"schema_version":1,"components":[{"id":"mp3","type":"webradio","x":0,"y":0,"width":400,"height":300,"config":{"name":"MP3 test","stream_url":format!("http://{addr}/mp3")}},{"id":"aac","type":"webradio","x":500,"y":0,"width":400,"height":300,"config":{"name":"AAC test","stream_url":format!("http://{addr}/aac")}}]})).unwrap();
    eprintln!("activate");
    e.request(Operation::Activate(scene.clone())).await.unwrap();
    e.request(Operation::Command("mp3".into(), Command::Play))
        .await
        .unwrap();
    wait_state(&e, "mp3", "playing").await;
    eprintln!("MP3 playing");
    assert!(counts.samples.load(Ordering::SeqCst) > 0);
    let opened = counts.opened.load(Ordering::SeqCst);
    let patch: Patch =
        serde_json::from_value(json!({"x":30,"config":{"name":"Renamed","volume":0.2}})).unwrap();
    e.request(Operation::Update("mp3".into(), patch))
        .await
        .unwrap();
    assert_eq!(counts.opened.load(Ordering::SeqCst), opened);
    eprintln!("claim AAC, injecting open failure");
    counts.fail_next.store(true, Ordering::SeqCst);
    e.request(Operation::Command("aac".into(), Command::Play))
        .await
        .unwrap();
    wait_state(&e, "aac", "error").await;
    assert_eq!(
        e.request(Operation::Status("aac".into())).await.unwrap()["error"],
        "simulated device-open error"
    );
    eprintln!("AAC retry");
    assert_eq!(counts.active.load(Ordering::SeqCst), 0);
    wait_state(&e, "aac", "playing").await;
    eprintln!("AAC playing, replacing scene");
    assert_eq!(
        e.request(Operation::Status("mp3".into())).await.unwrap()["state"],
        "stopped"
    );
    assert_eq!(counts.peak.load(Ordering::SeqCst), 1);
    assert!(counts.opened.load(Ordering::SeqCst) >= 2);
    e.request(Operation::Activate(scene)).await.unwrap();
    assert_eq!(counts.active.load(Ordering::SeqCst), 0);
    tokio::time::timeout(Duration::from_secs(2), async {
        while counts.connections.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    eprintln!("engine and server shutdown");
    e.shutdown();
    tokio::time::timeout(Duration::from_secs(2), t)
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
