use crate::{
    engine::{EngineHandle, Error, Operation},
    model::{Command, Patch, Scene},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRequest, Path, Request, State},
    http::StatusCode,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post, put},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
#[derive(Clone)]
struct Api {
    engine: EngineHandle,
    sse: Arc<Semaphore>,
    requests: Arc<Semaphore>,
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}
pub struct Body<T>(T);
impl<S, T> FromRequest<S> for Body<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Error;
    async fn from_request(req: Request, state: &S) -> Result<Self, Error> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Self(v))
            .map_err(|e| Error {
                status: e.status().as_u16(),
                code: "invalid_json",
                message: e.body_text(),
            })
    }
}
pub fn router(engine: EngineHandle) -> Router {
    let state = Api {
        engine,
        sse: Arc::new(Semaphore::new(16)),
        requests: Arc::new(Semaphore::new(32)),
    };
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(server))
        .route("/v1/scene", put(activate).get(scene).delete(stop))
        .route("/v1/components", get(components))
        .route("/v1/components/{id}", get(component).patch(update))
        .route("/v1/components/{id}/status", get(status))
        .route("/v1/components/{id}/commands", post(command))
        .route("/v1/commands", post(addressed_command))
        .route("/v1/audio", get(audio))
        .route("/v1/events", get(events))
        .fallback(|| async {
            Error {
                status: 404,
                code: "not_found",
                message: "endpoint not found".into(),
            }
        })
        .method_not_allowed_fallback(|| async {
            Error {
                status: 405,
                code: "method_not_allowed",
                message: "method not allowed".into(),
            }
        })
        .layer(DefaultBodyLimit::max(65536))
        .layer(axum::middleware::from_fn_with_state(state.clone(), limit))
        .with_state(state)
}
async fn limit(State(s): State<Api>, request: Request, next: axum::middleware::Next) -> Response {
    let Ok(_permit) = s.requests.try_acquire() else {
        return Error {
            status: 503,
            code: "busy",
            message: "too many concurrent requests".into(),
        }
        .into_response();
    };
    match tokio::time::timeout(Duration::from_secs(30), next.run(request)).await {
        Ok(r) => r,
        Err(_) => Error {
            status: 408,
            code: "timeout",
            message: "request exceeded 30 seconds; query status before retrying mutations".into(),
        }
        .into_response(),
    }
}
async fn execute(s: Api, op: Operation) -> Result<Json<Value>, Error> {
    s.engine.request(op).await.map(Json)
}
async fn activate(State(s): State<Api>, Body(v): Body<Scene>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Activate(v)).await
}
async fn scene(State(s): State<Api>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Scene).await
}
async fn stop(State(s): State<Api>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Stop).await
}
async fn components(State(s): State<Api>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Components).await
}
async fn component(State(s): State<Api>, Path(id): Path<String>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Component(id)).await
}
async fn update(
    State(s): State<Api>,
    Path(id): Path<String>,
    Body(v): Body<Patch>,
) -> Result<Json<Value>, Error> {
    execute(s, Operation::Update(id, v)).await
}
async fn command(
    State(s): State<Api>,
    Path(id): Path<String>,
    Body(v): Body<Command>,
) -> Result<Json<Value>, Error> {
    execute(s, Operation::Command(id, v)).await
}
async fn addressed_command(
    State(s): State<Api>,
    Body(mut value): Body<Value>,
) -> Result<Json<Value>, Error> {
    let obj = value
        .as_object_mut()
        .ok_or_else(|| Error::invalid("command must be an object"))?;
    let id = obj
        .remove("id")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| Error::invalid("id is required"))?;
    let command = serde_json::from_value(value).map_err(|e| Error::invalid(e.to_string()))?;
    execute(s, Operation::Command(id, command)).await
}
async fn status(State(s): State<Api>, Path(id): Path<String>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Status(id)).await
}
async fn audio(State(s): State<Api>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Audio).await
}
async fn server(State(s): State<Api>) -> Result<Json<Value>, Error> {
    execute(s, Operation::Server).await
}
async fn health(State(s): State<Api>) -> Result<Json<Value>, Error> {
    let status = s.engine.request(Operation::Server).await?;
    if !status["display_error"].is_null() {
        return Err(Error {
            status: 503,
            code: "display_error",
            message: status["display_error"]
                .as_str()
                .unwrap_or("display failed")
                .into(),
        });
    }
    Ok(Json(json!({"status":"ok"})))
}
async fn events(State(s): State<Api>) -> Result<impl IntoResponse, Error> {
    let permit = s.sse.clone().try_acquire_owned().map_err(|_| Error {
        status: 503,
        code: "sse_limit",
        message: "at most 16 SSE subscribers".into(),
    })?;
    let receiver = s.engine.events.subscribe();
    let stream = futures_util::stream::unfold(
        (receiver, permit, true, s.engine.cancellation()),
        |(mut rx, permit, initial, cancel)| async move {
            let event = if initial {
                Event::default()
                    .event("resync")
                    .data("{\"reason\":\"connected\",\"status_url\":\"/v1/status\"}")
                    .retry(Duration::from_secs(2))
            } else {
                match tokio::select! { _ = cancel.cancelled() => return None, event = rx.recv() => event }
                {
                    Ok(e) => Event::default()
                        .id(e.sequence.to_string())
                        .event(e.event)
                        .json_data(e.data)
                        .expect("JSON event"),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => Event::default()
                        .event("resync")
                        .data(format!("{{\"reason\":\"lagged\",\"missed\":{n}}}")),
                    Err(_) => return None,
                }
            };
            Some((Ok::<_, Infallible>(event), (rx, permit, false, cancel)))
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
