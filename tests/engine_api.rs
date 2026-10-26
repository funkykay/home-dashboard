use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use pi_dashboard::{
    api,
    audio::output::NullFactory,
    engine::{Engine, EngineHandle, Operation},
    model::{Command, Patch, Scene},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
fn scene() -> Scene {
    serde_json::from_value(json!({"schema_version":1,"components":[{"id":"a","type":"webradio","x":0,"y":0,"width":500,"height":300,"config":{"name":"Radio A","stream_url":"http://127.0.0.1:9/radio"}},{"id":"b","type":"webradio","x":500,"y":0,"width":500,"height":300,"config":{"name":"Radio B","stream_url":"http://127.0.0.1:9/radio"}}]})).unwrap()
}
fn engine() -> (EngineHandle, tokio::task::JoinHandle<()>) {
    Engine::spawn(None, Arc::new(NullFactory), "test".into())
}
async fn end(e: EngineHandle, t: tokio::task::JoinHandle<()>) {
    e.shutdown();
    tokio::time::timeout(Duration::from_secs(3), t)
        .await
        .unwrap()
        .unwrap();
}
#[test]
fn scene_validation() {
    let original = scene();
    assert!(original.validate().is_ok());
    let mut s = original.clone();
    s.schema_version = 2;
    assert!(s.validate().is_err());
    let mut s = original.clone();
    s.components[1].id = "a".into();
    assert!(s.validate().is_err());
    let mut s = original.clone();
    s.components[0].x = u32::MAX;
    assert!(s.validate().is_err());
    let mut s = original.clone();
    s.components[0].style.background = "#ZZ00FF".into();
    assert!(s.validate().is_err());
    let mut s = original.clone();
    s.components[0].style.font_size = 0;
    assert!(s.validate().is_err());
    let mut v = serde_json::to_value(original).unwrap();
    v["components"][0]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<Scene>(v).is_err());
}
#[tokio::test]
async fn invalid_replacement_is_atomic_and_updates_preserve_others() {
    let (e, t) = engine();
    assert_eq!(e.request(Operation::Scene).await.unwrap(), Value::Null);
    e.request(Operation::Activate(scene())).await.unwrap();
    e.request(Operation::Command("b".into(), Command::Pause))
        .await
        .unwrap();
    let before = e.request(Operation::Scene).await.unwrap();
    let mut bad = scene();
    bad.components[0].width = 9999;
    assert_eq!(
        e.request(Operation::Activate(bad))
            .await
            .unwrap_err()
            .status,
        422
    );
    assert_eq!(before, e.request(Operation::Scene).await.unwrap());
    let patch: Patch =
        serde_json::from_value(json!({"x":20,"style":{"font_size":24},"config":{"name":"Neu"}}))
            .unwrap();
    e.request(Operation::Update("a".into(), patch))
        .await
        .unwrap();
    assert_eq!(
        e.request(Operation::Status("b".into())).await.unwrap()["state"],
        "paused"
    );
    let invalid: Patch = serde_json::from_value(json!({"config":{"volume":3}})).unwrap();
    assert!(
        e.request(Operation::Update("a".into(), invalid))
            .await
            .is_err()
    );
    e.request(Operation::Activate(scene())).await.unwrap();
    assert_eq!(
        e.request(Operation::Status("b".into())).await.unwrap()["state"],
        "stopped"
    );
    e.request(Operation::Stop).await.unwrap();
    assert_eq!(e.request(Operation::Scene).await.unwrap(), Value::Null);
    end(e, t).await;
}
#[tokio::test]
async fn concurrent_claims_serialize_and_cleanup() {
    let (e, t) = engine();
    e.request(Operation::Activate(scene())).await.unwrap();
    let mut jobs = Vec::new();
    for i in 0..30 {
        let e = e.clone();
        jobs.push(tokio::spawn(async move {
            e.request(Operation::Command(
                if i % 2 == 0 { "a" } else { "b" }.into(),
                Command::Play,
            ))
            .await
            .unwrap()
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    let owner = e.request(Operation::Audio).await.unwrap()["owner"]
        .as_str()
        .unwrap()
        .to_string();
    let other = if owner == "a" { "b" } else { "a" };
    assert_eq!(
        e.request(Operation::Status(other.into())).await.unwrap()["state"],
        "stopped"
    );
    let before = e.request(Operation::Audio).await.unwrap();
    e.request(Operation::Command(owner.clone(), Command::Play))
        .await
        .unwrap();
    assert_eq!(before, e.request(Operation::Audio).await.unwrap());
    e.request(Operation::Command(owner, Command::Stop))
        .await
        .unwrap();
    assert!(e.request(Operation::Audio).await.unwrap()["owner"].is_null());
    e.request(Operation::Command(
        "a".into(),
        Command::Volume { value: 0.25 },
    ))
    .await
    .unwrap();
    assert_eq!(
        e.request(Operation::Status("a".into())).await.unwrap()["volume"],
        0.25
    );
    assert!(
        e.request(Operation::Command(
            "a".into(),
            Command::Volume { value: f32::NAN }
        ))
        .await
        .is_err()
    );
    e.request(Operation::Stop).await.unwrap();
    end(e, t).await;
}
async fn http(app: axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let r = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn api_validation_commands_and_status() {
    let (e, t) = engine();
    let app = api::router(e.clone());
    assert_eq!(
        http(app.clone(), "GET", "/health", Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        http(
            app.clone(),
            "PUT",
            "/v1/scene",
            serde_json::to_value(scene()).unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        http(
            app.clone(),
            "POST",
            "/v1/commands",
            json!({"id":"a","command":"pause"})
        )
        .await
        .1["state"],
        "paused"
    );
    assert_eq!(
        http(
            app.clone(),
            "POST",
            "/v1/components/a/commands",
            json!({"command":"volume","value":2})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        http(app.clone(), "PATCH", "/v1/components/a", json!({"bogus":5}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        http(app.clone(), "GET", "/v1/components/missing", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let huge = json!({"padding":"x".repeat(70000)});
    let (status, value) = http(app.clone(), "PUT", "/v1/scene", huge).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(value["error"]["code"], "invalid_json");
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/scene")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        http(app, "DELETE", "/v1/scene", Value::Null).await.0,
        StatusCode::OK
    );
    end(e, t).await;
}
#[tokio::test]
async fn sse_events_resync_lag_limit_and_shutdown() {
    let (e, t) = engine();
    let app = api::router(e.clone());
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let mut body = r.into_body();
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("event: resync"));
    e.request(Operation::Activate(scene())).await.unwrap();
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("scene.activated"));
    for _ in 0..140 {
        e.request(Operation::Stop).await.unwrap();
    }
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("lagged"));
    let mut connections = Vec::new();
    for _ in 0..15 {
        connections.push(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri("/v1/events")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/events")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(connections);
    drop(body);
    end(e, t).await;
}
#[tokio::test]
async fn radio_error_isolated_and_retry_cancelled() {
    let (e, t) = engine();
    e.request(Operation::Activate(scene())).await.unwrap();
    let mut events = e.events.subscribe();
    e.request(Operation::Command("a".into(), Command::Play))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if events.recv().await.unwrap().event == "webradio.playback_error" {
                break;
            }
        }
    })
    .await
    .unwrap();
    e.request(Operation::Command("b".into(), Command::Pause))
        .await
        .unwrap();
    e.request(Operation::Stop).await.unwrap();
    let mut events = e.events.subscribe();
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), events.recv())
            .await
            .is_err()
    );
    end(e, t).await;
}
