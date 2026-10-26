use crate::{
    audio::{AudioManager, output::OutputFactory},
    component::{self, Action, Component},
    model::{Command, Config, Patch, Scene},
    render::RenderHandle,
    weather::{WeatherData, WeatherService},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum Internal {
    Weather {
        id: String,
        generation: u64,
        data: Result<Arc<WeatherData>, String>,
    },
    Playback {
        id: String,
        generation: u64,
        session: u64,
        state: String,
        error: Option<String>,
    },
    Metadata {
        id: String,
        generation: u64,
        session: u64,
        title: String,
    },
    OwnershipLost {
        id: String,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct External {
    pub sequence: u64,
    pub event: String,
    pub data: Value,
}
#[derive(Clone, Debug)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 422,
            code: "invalid_configuration",
            message: message.into(),
        }
    }
    pub fn missing() -> Self {
        Self {
            status: 404,
            code: "not_found",
            message: "component not found".into(),
        }
    }
}
pub enum Operation {
    Activate(Scene),
    Stop,
    Scene,
    Components,
    Component(String),
    Status(String),
    Update(String, Patch),
    Command(String, Command),
    Audio,
    Server,
}
struct Request {
    operation: Operation,
    reply: oneshot::Sender<Result<Value, Error>>,
}
#[derive(Clone)]
pub struct EngineHandle {
    requests: mpsc::Sender<Request>,
    pub events: broadcast::Sender<External>,
    cancel: CancellationToken,
}
impl EngineHandle {
    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }
    pub async fn request(&self, operation: Operation) -> Result<Value, Error> {
        let (reply, rx) = oneshot::channel();
        self.requests
            .try_send(Request { operation, reply })
            .map_err(|e| Error {
                status: 503,
                code: "unavailable",
                message: format!("engine unavailable: {e}"),
            })?;
        rx.await.map_err(|_| Error {
            status: 503,
            code: "unavailable",
            message: "engine stopped".into(),
        })?
    }
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}
pub struct Engine;
impl Engine {
    pub fn spawn(
        render: Option<RenderHandle>,
        factory: Arc<dyn OutputFactory>,
        backend: String,
    ) -> (EngineHandle, tokio::task::JoinHandle<()>) {
        let (requests, rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(128);
        let (internal, irx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        let weather_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("pi-dashboard/0.1 (home dashboard)")
            .build()
            .expect("TLS client");
        let audio_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .connect_timeout(std::time::Duration::from_secs(8))
            .read_timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("pi-dashboard/0.1")
            .build()
            .expect("TLS client");
        let state = State {
            components: Vec::new(),
            active: false,
            generation: 0,
            sequence: 0,
            events: events.clone(),
            internal: internal.clone(),
            weather: Arc::new(WeatherService::new(weather_client)),
            audio: AudioManager::new(factory, audio_client, internal),
            render,
            backend,
        };
        let task = tokio::spawn(state.run(rx, irx, cancel.clone()));
        (
            EngineHandle {
                requests,
                events,
                cancel,
            },
            task,
        )
    }
}
struct State {
    components: Vec<Box<dyn Component>>,
    active: bool,
    generation: u64,
    sequence: u64,
    events: broadcast::Sender<External>,
    internal: mpsc::Sender<Internal>,
    weather: Arc<WeatherService>,
    audio: AudioManager,
    render: Option<RenderHandle>,
    backend: String,
}
impl State {
    async fn run(
        mut self,
        mut requests: mpsc::Receiver<Request>,
        mut internal: mpsc::Receiver<Internal>,
        cancel: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _=cancel.cancelled()=>break,
                request=requests.recv()=>{let Some(r)=request else{break;};let result=self.handle(r.operation).await;let _=r.reply.send(result);},
                Some(event)=internal.recv()=>self.internal(event),
            }
        }
        self.clear().await;
    }
    fn emit(&mut self, name: &str, data: Value) {
        self.sequence += 1;
        let _ = self.events.send(External {
            sequence: self.sequence,
            event: name.into(),
            data,
        });
    }
    fn repaint(&self) {
        if let Some(r) = &self.render {
            r.submit(self.components.iter().map(|c| c.render()).collect());
        }
    }
    fn scene(&self) -> Value {
        if self.active {
            json!(Scene {
                schema_version: 1,
                components: self.components.iter().map(|c| c.spec().clone()).collect()
            })
        } else {
            Value::Null
        }
    }
    fn index(&self, id: &str) -> Result<usize, Error> {
        self.components
            .iter()
            .position(|c| c.spec().id == id)
            .ok_or_else(Error::missing)
    }
    async fn release_audio(&mut self) {
        if let Some(id) = self.audio.release().await {
            if let Ok(i) = self.index(&id) {
                self.components[i].event(&Internal::OwnershipLost { id: id.clone() });
                self.emit(
                    "component.state_changed",
                    json!({"id":id,"status":self.components[i].status()}),
                );
            }
            self.emit("audio.owner_changed", json!({"owner":null}));
        }
    }
    async fn clear(&mut self) {
        self.release_audio().await;
        for c in &mut self.components {
            if let Some(worker) = c.detach() {
                worker.stop().await;
            }
        }
        self.components.clear();
        self.active = false;
    }
    async fn handle(&mut self, op: Operation) -> Result<Value, Error> {
        match op {
            Operation::Activate(scene) => {
                scene.validate().map_err(Error::invalid)?;
                self.clear().await;
                for spec in scene.components {
                    self.generation += 1;
                    self.components.push(component::create(
                        spec,
                        self.generation,
                        self.weather.clone(),
                        self.internal.clone(),
                    ));
                }
                self.active = true;
                self.repaint();
                self.emit(
                    "scene.activated",
                    json!({"component_count":self.components.len()}),
                );
                Ok(self.scene())
            }
            Operation::Stop => {
                self.clear().await;
                self.repaint();
                self.emit("scene.stopped", json!({}));
                Ok(json!({"active":false}))
            }
            Operation::Scene => Ok(self.scene()),
            Operation::Components => Ok(json!(
                self.components
                    .iter()
                    .map(|c| json!({"component":c.spec(),"status":c.status()}))
                    .collect::<Vec<_>>()
            )),
            Operation::Component(id) => {
                let i = self.index(&id)?;
                Ok(json!(self.components[i].spec()))
            }
            Operation::Status(id) => {
                let i = self.index(&id)?;
                Ok(self.components[i].status())
            }
            Operation::Update(id, patch) => {
                let i = self.index(&id)?;
                let next = patch
                    .apply(self.components[i].spec())
                    .map_err(Error::invalid)?;
                if component::restart_required(self.components[i].spec(), &next) {
                    if self.audio.owner.as_ref() == Some(&id) {
                        self.release_audio().await;
                    }
                    if let Some(worker) = self.components[i].detach() {
                        worker.stop().await;
                    }
                    self.generation += 1;
                    self.components[i] = component::create(
                        next,
                        self.generation,
                        self.weather.clone(),
                        self.internal.clone(),
                    );
                } else {
                    if self.audio.owner.as_ref() == Some(&id)
                        && let Config::Webradio(r) = &next.kind
                    {
                        self.audio.volume(r.volume);
                    }
                    self.components[i].update(next);
                }
                self.repaint();
                self.emit("component.updated",json!({"id":id,"component":self.components[i].spec(),"status":self.components[i].status()}));
                Ok(json!(self.components[i].spec()))
            }
            Operation::Command(id, command) => {
                let i = self.index(&id)?;
                let action = self.components[i]
                    .command(command)
                    .map_err(Error::invalid)?;
                match action {
                    Action::Play => {
                        if self.audio.owner.as_ref() == Some(&id) {
                            return Ok(self.components[i].status());
                        }
                        if self.audio.owner.as_ref() != Some(&id) {
                            self.release_audio().await;
                            let c = &self.components[i];
                            let Config::Webradio(config) = &c.spec().kind else {
                                unreachable!()
                            };
                            self.audio
                                .claim(id.clone(), c.generation(), config.clone())
                                .await;
                            self.emit("audio.owner_changed", json!({"owner":id}));
                        }
                        let generation = self.components[i].generation();
                        self.components[i].event(&Internal::Playback {
                            id: id.clone(),
                            generation,
                            session: self.audio.session,
                            state: "buffering".into(),
                            error: None,
                        });
                    }
                    Action::Pause | Action::Stop => {
                        if self.audio.owner.as_ref() == Some(&id) {
                            self.release_audio().await;
                        }
                        let generation = self.components[i].generation();
                        self.components[i].event(&Internal::Playback {
                            id: id.clone(),
                            generation,
                            session: self.audio.session,
                            state: if matches!(action, Action::Pause) {
                                "paused"
                            } else {
                                "stopped"
                            }
                            .into(),
                            error: None,
                        });
                    }
                    Action::Volume(value) => {
                        let mut spec = self.components[i].spec().clone();
                        if let Config::Webradio(c) = &mut spec.kind {
                            c.volume = value;
                        }
                        self.components[i].update(spec);
                        if self.audio.owner.as_ref() == Some(&id) {
                            self.audio.volume(value);
                        }
                    }
                }
                self.repaint();
                let status = self.components[i].status();
                self.emit("component.state_changed", json!({"id":id,"status":status}));
                Ok(status)
            }
            Operation::Audio => Ok(json!({"owner":self.audio.owner,"session":self.audio.session})),
            Operation::Server => {
                let (frames, pixels, error) = if let Some(r) = &self.render {
                    (
                        r.stats.frames.load(std::sync::atomic::Ordering::Relaxed),
                        r.stats.pixels.load(std::sync::atomic::Ordering::Relaxed),
                        r.stats.error.lock().unwrap().clone(),
                    )
                } else {
                    (0, 0, None)
                };
                Ok(
                    json!({"version":env!("CARGO_PKG_VERSION"),"scene_active":self.active,"components":self.components.len(),"audio_owner":self.audio.owner,"display":self.backend,"display_error":error,"rendered_frames":frames,"rendered_pixels":pixels,"event_sequence":self.sequence}),
                )
            }
        }
    }
    fn internal(&mut self, event: Internal) {
        let (id, generation, session) = match &event {
            Internal::Weather { id, generation, .. } => (id, *generation, None),
            Internal::Playback {
                id,
                generation,
                session,
                ..
            }
            | Internal::Metadata {
                id,
                generation,
                session,
                ..
            } => (id, *generation, Some(*session)),
            Internal::OwnershipLost { .. } => return,
        };
        let Ok(i) = self.index(id) else {
            return;
        };
        if self.components[i].generation() != generation {
            return;
        }
        if let Some(session) = session
            && (self.audio.session != session || self.audio.owner.as_ref() != Some(id))
        {
            return;
        }
        if self.components[i].event(&event) {
            let id = id.clone();
            self.repaint();
            let status = self.components[i].status();
            self.emit("component.state_changed", json!({"id":id,"status":status}));
            if matches!(event, Internal::Playback { error: Some(_), .. }) {
                self.emit("webradio.playback_error", json!({"id":id,"status":status}));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounded_request_channel_rejects_overload_and_shutdown_joins() {
        let (engine, task) = Engine::spawn(
            None,
            Arc::new(crate::audio::output::NullFactory),
            "test".into(),
        );
        let mut replies = Vec::new();
        // This current-thread runtime cannot run the actor before the first await.
        for _ in 0..64 {
            let (reply, rx) = oneshot::channel();
            engine
                .requests
                .try_send(Request {
                    operation: Operation::Server,
                    reply,
                })
                .unwrap();
            replies.push(rx);
        }
        assert_eq!(
            engine.request(Operation::Server).await.unwrap_err().status,
            503
        );
        for rx in replies {
            assert!(rx.await.unwrap().is_ok());
        }
        engine.shutdown();
        task.await.unwrap();
        assert_eq!(
            engine.request(Operation::Server).await.unwrap_err().status,
            503
        );
    }
    #[tokio::test]
    async fn stale_internal_messages_cannot_revive_removed_owners() {
        let (events, _) = broadcast::channel(16);
        let (internal, _) = mpsc::channel(16);
        let client = reqwest::Client::new();
        let mut s = State {
            components: Vec::new(),
            active: false,
            generation: 0,
            sequence: 0,
            events,
            internal: internal.clone(),
            weather: Arc::new(WeatherService::new(client.clone())),
            audio: AudioManager::new(
                Arc::new(crate::audio::output::NullFactory),
                client,
                internal,
            ),
            render: None,
            backend: "test".into(),
        };
        let scene:Scene=serde_json::from_value(json!({"schema_version":1,"components":[{"id":"radio","type":"webradio","x":0,"y":0,"width":400,"height":200,"config":{"name":"Radio","stream_url":"http://localhost:9/"}}]})).unwrap();
        s.handle(Operation::Activate(scene)).await.unwrap();
        s.audio.owner = Some("radio".into());
        s.audio.session = 2;
        for (generation, session) in [(0, 2), (1, 1)] {
            s.internal(Internal::Playback {
                id: "radio".into(),
                generation,
                session,
                state: "playing".into(),
                error: None,
            });
            assert_eq!(s.components[0].status()["state"], "stopped");
        }
        s.internal(Internal::Playback {
            id: "radio".into(),
            generation: 1,
            session: 2,
            state: "playing".into(),
            error: None,
        });
        assert_eq!(s.components[0].status()["state"], "playing");
        s.handle(Operation::Stop).await.unwrap();
        s.internal(Internal::Playback {
            id: "radio".into(),
            generation: 1,
            session: 2,
            state: "playing".into(),
            error: None,
        });
        assert!(!s.active);
        assert!(s.audio.owner.is_none());
        assert!(s.components.is_empty());
    }
}
