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

// Gate actual Output destruction, so completion cannot be confused with cancellation.
struct CleanupGate {
    open: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
    entered: tokio::sync::Notify,
    panic: bool,
}
impl CleanupGate {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
struct GatedFactory {
    counts: Arc<Counts>,
    next: std::sync::Mutex<Option<Arc<CleanupGate>>>,
}
struct GatedSink {
    sink: Sink,
    gate: Option<Arc<CleanupGate>>,
}
impl OutputFactory for GatedFactory {
    fn open(&self, _: u32, _: u32) -> Result<Box<dyn Output>> {
        let n = self.counts.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.counts.peak.fetch_max(n, Ordering::SeqCst);
        self.counts.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(GatedSink {
            sink: Sink(self.counts.clone()),
            gate: self.next.lock().unwrap().take(),
        }))
    }
}
impl Output for GatedSink {
    fn write(&mut self, data: &[i16], cancel: &CancellationToken) -> Result<()> {
        self.sink.write(data, cancel)
    }
}
impl Drop for GatedSink {
    fn drop(&mut self) {
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            let mut open = gate.open.lock().unwrap();
            while !*open {
                open = gate.wake.wait(open).unwrap();
            }
            drop(open);
            assert!(!gate.panic, "simulated cleanup panic");
        }
    }
}
struct Handoff {
    engine: EngineHandle,
    task: tokio::task::JoinHandle<()>,
    server: tokio::task::JoinHandle<()>,
    stop: CancellationToken,
    counts: Arc<Counts>,
    gate: Arc<CleanupGate>,
    scene: Scene,
}
impl Drop for Handoff {
    fn drop(&mut self) {
        self.gate.release();
        self.engine.shutdown();
        self.stop.cancel();
    }
}
impl Handoff {
    async fn start(panic: bool) -> Self {
        let counts = Arc::new(Counts::default());
        let c = counts.clone();
        let app = Router::new().route(
            "/",
            get(move || stream(include_bytes!("fixtures/tone.mp3"), c.clone())),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(token.cancelled_owned())
                .await
                .unwrap();
        });
        let gate = Arc::new(CleanupGate {
            open: std::sync::Mutex::new(false),
            wake: std::sync::Condvar::new(),
            entered: tokio::sync::Notify::new(),
            panic,
        });
        let (engine, task) = Engine::spawn(
            None,
            Arc::new(GatedFactory {
                counts: counts.clone(),
                next: std::sync::Mutex::new(Some(gate.clone())),
            }),
            "test".into(),
        );
        let scene: Scene = serde_json::from_value(json!({"schema_version":1,"components":[
            {"id":"a","type":"webradio","x":0,"y":0,"width":400,"height":200,"config":{"name":"A","stream_url":format!("http://{addr}/")}},
            {"id":"b","type":"webradio","x":400,"y":0,"width":400,"height":200,"config":{"name":"B","stream_url":format!("http://{addr}/")}}
        ]})).unwrap();
        engine
            .request(Operation::Activate(scene.clone()))
            .await
            .unwrap();
        engine
            .request(Operation::Command("a".into(), Command::Play))
            .await
            .unwrap();
        wait_state(&engine, "a", "playing").await;
        Self {
            engine,
            task,
            server,
            stop,
            counts,
            gate,
            scene,
        }
    }
    async fn finish(&mut self) {
        self.gate.release();
        self.engine.shutdown();
        tokio::time::timeout(Duration::from_secs(3), &mut self.task)
            .await
            .unwrap()
            .unwrap();
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(3), &mut self.server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(self.counts.active.load(Ordering::SeqCst), 0);
        assert_eq!(self.counts.connections.load(Ordering::SeqCst), 0);
        assert_eq!(self.counts.peak.load(Ordering::SeqCst), 1);
    }
}
async fn queued(
    e: &EngineHandle,
    op: Operation,
) -> tokio::task::JoinHandle<Result<serde_json::Value, pi_dashboard::engine::Error>> {
    let e = e.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let job = tokio::spawn(async move {
        let mut request = Box::pin(e.request(op));
        assert!(futures_util::poll!(&mut request).is_pending());
        tx.send(()).unwrap();
        request.await
    });
    rx.await.unwrap();
    job
}
#[tokio::test]
async fn slow_cleanup_keeps_status_and_unrelated_updates_responsive_and_claims_fifo() {
    let mut h = Handoff::start(false).await;
    let b = queued(&h.engine, Operation::Command("b".into(), Command::Play)).await;
    h.gate.entered.notified().await;
    let a = queued(&h.engine, Operation::Command("a".into(), Command::Play)).await;
    tokio::time::timeout(Duration::from_millis(500), async {
        assert_eq!(
            h.engine.request(Operation::Audio).await.unwrap()["releasing"],
            true
        );
        h.engine.request(Operation::Server).await.unwrap();
        h.engine
            .request(Operation::Status("b".into()))
            .await
            .unwrap();
        h.engine
            .request(Operation::Update(
                "b".into(),
                serde_json::from_value(json!({"config":{"name":"Renamed"}})).unwrap(),
            ))
            .await
            .unwrap();
    })
    .await
    .unwrap();
    assert!(!b.is_finished() && !a.is_finished());
    assert_eq!(h.counts.opened.load(Ordering::SeqCst), 1);
    h.gate.release();
    b.await.unwrap().unwrap();
    a.await.unwrap().unwrap();
    assert_eq!(
        h.engine.request(Operation::Audio).await.unwrap()["owner"],
        "a"
    );
    assert_eq!(
        h.engine
            .request(Operation::Status("b".into()))
            .await
            .unwrap()["state"],
        "stopped"
    );
    h.finish().await;
}
#[tokio::test]
async fn scene_replacement_during_handoff_joins_workers_and_rejects_old_events() {
    let mut h = Handoff::start(false).await;
    let b = queued(&h.engine, Operation::Command("b".into(), Command::Play)).await;
    h.gate.entered.notified().await;
    let replace = queued(&h.engine, Operation::Activate(h.scene.clone())).await;
    h.gate.release();
    b.await.unwrap().unwrap();
    replace.await.unwrap().unwrap();
    assert_eq!(h.counts.active.load(Ordering::SeqCst), 0);
    assert!(h.engine.request(Operation::Audio).await.unwrap()["owner"].is_null());
    for id in ["a", "b"] {
        assert_eq!(
            h.engine
                .request(Operation::Status(id.into()))
                .await
                .unwrap()["state"],
            "stopped"
        );
    }
    h.finish().await;
}
#[tokio::test]
async fn shutdown_during_handoff_cancels_queued_claim_and_joins_cleanup() {
    let mut h = Handoff::start(false).await;
    let b = queued(&h.engine, Operation::Command("b".into(), Command::Play)).await;
    h.gate.entered.notified().await;
    h.engine.shutdown();
    assert_eq!(b.await.unwrap().unwrap_err().status, 503);
    assert!(!h.task.is_finished());
    h.finish().await;
    assert_eq!(h.counts.opened.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cleanup_failure_is_reported_after_all_workers_join() {
    let mut h = Handoff::start(true).await;
    let b = queued(&h.engine, Operation::Command("b".into(), Command::Play)).await;
    h.gate.entered.notified().await;
    h.gate.release();
    assert_eq!(b.await.unwrap().unwrap_err().status, 503);
    let audio = h.engine.request(Operation::Audio).await.unwrap();
    assert!(
        audio["error"]
            .as_str()
            .unwrap()
            .contains("output cleanup panicked")
    );
    assert!(audio["owner"].is_null());
    assert_eq!(h.counts.active.load(Ordering::SeqCst), 0);
    // A joined worker panic is recoverable; an unjoined supervisor panic is not.
    h.engine
        .request(Operation::Command("b".into(), Command::Play))
        .await
        .unwrap();
    wait_state(&h.engine, "b", "playing").await;
    h.finish().await;
}
#[tokio::test]
async fn stuck_cleanup_reports_timeout_keeps_reservation_and_bounds_queue() {
    let mut h = Handoff::start(false).await;
    let first = queued(&h.engine, Operation::Command("b".into(), Command::Play)).await;
    h.gate.entered.notified().await;
    let mut pending = vec![first];
    for _ in 1..64 {
        pending.push(queued(&h.engine, Operation::Command("b".into(), Command::Play)).await);
    }
    assert_eq!(
        h.engine
            .request(Operation::Command("b".into(), Command::Play))
            .await
            .unwrap_err()
            .status,
        503
    );
    tokio::time::timeout(Duration::from_secs(7), async {
        for request in pending {
            assert_eq!(request.await.unwrap().unwrap_err().status, 503);
        }
    })
    .await
    .unwrap();
    let audio = h.engine.request(Operation::Audio).await.unwrap();
    assert_eq!(audio["owner"], "a");
    assert_eq!(audio["releasing"], true);
    assert!(audio["error"].as_str().unwrap().contains("five seconds"));
    assert_eq!(
        h.engine
            .request(Operation::Command("b".into(), Command::Play))
            .await
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(h.counts.active.load(Ordering::SeqCst), 1);
    assert_eq!(h.counts.opened.load(Ordering::SeqCst), 1);
    h.finish().await;
}
