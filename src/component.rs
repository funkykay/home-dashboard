use crate::{
    engine::Internal,
    model::*,
    render::{Draw, RenderItem},
    weather::{WeatherData, WeatherService},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum Action {
    Play,
    Pause,
    Stop,
    Volume(f32),
}
pub struct Worker {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
impl Worker {
    pub async fn stop(mut self) {
        self.cancel.cancel();
        let _ = (&mut self.task).await;
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
// Lifecycle operations are synchronous except for the explicit joined worker returned by detach.
pub trait Component: Send {
    fn spec(&self) -> &ComponentSpec;
    fn generation(&self) -> u64;
    fn status(&self) -> Value;
    fn render(&self) -> RenderItem;
    fn update(&mut self, spec: ComponentSpec);
    fn event(&mut self, event: &Internal) -> bool;
    fn command(&mut self, command: Command) -> Result<Action, String>;
    fn detach(&mut self) -> Option<Worker> {
        None
    }
}
pub fn validate(spec: &ComponentSpec) -> Result<(), String> {
    match &spec.kind {
        Config::Weather(c) => {
            if !c.latitude.is_finite()
                || !(-90.0..=90.0).contains(&c.latitude)
                || !c.longitude.is_finite()
                || !(-180.0..=180.0).contains(&c.longitude)
            {
                return Err("latitude must be -90..90; longitude -180..180".into());
            }
            if !(300..=86400).contains(&c.refresh_seconds) {
                return Err("refresh_seconds must be 300..86400".into());
            }
        }
        Config::Webradio(c) => {
            if c.name.trim().is_empty() || c.name.chars().count() > 128 {
                return Err("radio name must contain 1..128 characters".into());
            }
            let url = reqwest::Url::parse(&c.stream_url).map_err(|_| "invalid stream_url")?;
            if c.stream_url.len() > 2048
                || !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("stream_url must be an HTTP(S) URL without credentials".into());
            }
            validate_volume(c.volume)?;
        }
    }
    Ok(())
}
pub fn validate_volume(v: f32) -> Result<(), String> {
    if !v.is_finite() || !(0.0..=1.0).contains(&v) {
        Err("volume must be 0..1".into())
    } else {
        Ok(())
    }
}
pub fn restart_required(old: &ComponentSpec, new: &ComponentSpec) -> bool {
    match (&old.kind, &new.kind) {
        (Config::Weather(a), Config::Weather(b)) => a != b,
        (Config::Webradio(a), Config::Webradio(b)) => a.stream_url != b.stream_url,
        _ => true,
    }
}
pub fn create(
    spec: ComponentSpec,
    generation: u64,
    weather: Arc<WeatherService>,
    events: mpsc::Sender<Internal>,
) -> Box<dyn Component> {
    match &spec.kind {
        Config::Weather(config) => {
            let cancel = CancellationToken::new();
            let task = weather.spawn(
                spec.id.clone(),
                generation,
                config.clone(),
                events,
                cancel.clone(),
            );
            Box::new(Weather {
                spec,
                generation,
                revision: 0,
                data: None,
                error: None,
                worker: Some(Worker { cancel, task }),
            })
        }
        Config::Webradio(_) => Box::new(Radio {
            spec,
            generation,
            revision: 0,
            state: "stopped".into(),
            error: None,
            title: None,
        }),
    }
}
struct Weather {
    spec: ComponentSpec,
    generation: u64,
    revision: u64,
    data: Option<Arc<WeatherData>>,
    error: Option<String>,
    worker: Option<Worker>,
}
impl Component for Weather {
    fn spec(&self) -> &ComponentSpec {
        &self.spec
    }
    fn generation(&self) -> u64 {
        self.generation
    }
    fn status(&self) -> Value {
        json!({"state":if self.error.is_some(){"error"}else if self.data.is_some(){"ready"}else{"loading"},"error":self.error,"data":self.data.as_deref(),"stale":self.error.is_some() && self.data.is_some()})
    }
    fn update(&mut self, spec: ComponentSpec) {
        self.spec = spec;
        self.revision += 1;
    }
    fn render(&self) -> RenderItem {
        let mut item = panel(&self.spec, self.generation, self.revision);
        let mut lines = vec!["Wetter".into()];
        if let Some(d) = &self.data {
            lines.push(format!("{:.1} °C  ·  {}", d.temperature, d.condition));
            for f in &d.forecast {
                lines.push(format!(
                    "{}  {:.0}–{:.0} °C  {}",
                    f.date, f.minimum, f.maximum, f.condition
                ));
            }
        } else {
            lines.push("Wetterdaten werden geladen …".into());
        }
        if self.error.is_some() {
            lines.push("Verbindung gestört · erneuter Versuch folgt".into());
        }
        lines.push("Wetterdaten: Open-Meteo.com · CC BY 4.0".into());
        add_lines(&mut item, &self.spec, lines);
        item
    }
    fn event(&mut self, event: &Internal) -> bool {
        if let Internal::Weather { data, .. } = event {
            match data {
                Ok(d) => {
                    self.data = Some(d.clone());
                    self.error = None;
                }
                Err(e) => self.error = Some(e.clone()),
            };
            self.revision += 1;
            true
        } else {
            false
        }
    }
    fn command(&mut self, _: Command) -> Result<Action, String> {
        Err("weather does not support playback commands".into())
    }
    fn detach(&mut self) -> Option<Worker> {
        self.worker.take()
    }
}
struct Radio {
    spec: ComponentSpec,
    generation: u64,
    revision: u64,
    state: String,
    error: Option<String>,
    title: Option<String>,
}
impl Component for Radio {
    fn spec(&self) -> &ComponentSpec {
        &self.spec
    }
    fn generation(&self) -> u64 {
        self.generation
    }
    fn status(&self) -> Value {
        let Config::Webradio(c) = &self.spec.kind else {
            unreachable!()
        };
        json!({"state":self.state,"error":self.error,"title":self.title,"volume":c.volume})
    }
    fn update(&mut self, spec: ComponentSpec) {
        self.spec = spec;
        self.revision += 1;
    }
    fn render(&self) -> RenderItem {
        let Config::Webradio(c) = &self.spec.kind else {
            unreachable!()
        };
        let mut item = panel(&self.spec, self.generation, self.revision);
        let label = match self.state.as_str() {
            "playing" => "Wiedergabe",
            "paused" => "Pausiert",
            "buffering" => "Verbindung wird aufgebaut …",
            "error" => "Verbindung gestört · erneuter Versuch folgt",
            _ => "Gestoppt",
        };
        let mut lines = vec![
            c.name.clone(),
            label.into(),
            format!("Lautstärke: {:.0} %", c.volume * 100.0),
        ];
        if let Some(title) = &self.title {
            lines.push(title.clone());
        }
        add_lines(&mut item, &self.spec, lines);
        if self.state == "buffering" {
            item.animate = true;
        }
        item
    }
    fn event(&mut self, event: &Internal) -> bool {
        match event {
            Internal::Playback { state, error, .. } => {
                if self.state == *state && self.error == *error {
                    return false;
                }
                self.state = state.clone();
                self.error = error.clone();
            }
            Internal::Metadata { title, .. } => {
                if self.title.as_ref() == Some(title) {
                    return false;
                }
                self.title = Some(title.clone());
            }
            Internal::OwnershipLost { .. } => {
                self.state = "stopped".into();
                self.error = None;
                self.title = None;
            }
            _ => return false,
        }
        self.revision += 1;
        true
    }
    fn command(&mut self, command: Command) -> Result<Action, String> {
        Ok(match command {
            Command::Play => Action::Play,
            Command::Pause => Action::Pause,
            Command::Stop => Action::Stop,
            Command::Volume { value } => {
                validate_volume(value)?;
                Action::Volume(value)
            }
        })
    }
}
fn panel(spec: &ComponentSpec, generation: u64, revision: u64) -> RenderItem {
    RenderItem {
        id: spec.id.clone(),
        rect: spec.rect(),
        generation,
        revision,
        animate: false,
        draws: vec![Draw::Fill(
            spec.rect(),
            Color::parse(&spec.style.background).unwrap(),
        )],
    }
}
fn add_lines(item: &mut RenderItem, spec: &ComponentSpec, lines: Vec<String>) {
    let pad = u32::from(spec.style.padding);
    let s = f32::from(spec.style.font_size);
    let rect = spec.rect();
    for (i, text) in lines.into_iter().enumerate() {
        item.draws.push(Draw::Text {
            text,
            x: rect.x + pad,
            y: rect.y + pad + i as u32 * (s as u32 + 12),
            width: rect.width.saturating_sub(pad * 2),
            size: s,
            color: Color::parse(&spec.style.text_color).unwrap(),
            align: spec.style.align,
        });
    }
    item.draws.push(Draw::Line {
        from: (rect.x, rect.y),
        to: (rect.right().saturating_sub(1), rect.y),
        color: Color::parse(&spec.style.accent).unwrap(),
    });
}
