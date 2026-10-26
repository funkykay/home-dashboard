use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const MAX_COMPONENTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Rect {
    pub const SCREEN: Self = Self {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }
    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }
    pub fn intersection(self, b: Self) -> Option<Self> {
        let x = self.x.max(b.x);
        let y = self.y.max(b.y);
        let r = self.right().min(b.right());
        let d = self.bottom().min(b.bottom());
        (r > x && d > y).then(|| Self {
            x,
            y,
            width: r - x,
            height: d - y,
        })
    }
    pub fn union(self, b: Self) -> Self {
        let x = self.x.min(b.x);
        let y = self.y.min(b.y);
        Self {
            x,
            y,
            width: self.right().max(b.right()) - x,
            height: self.bottom().max(b.bottom()) - y,
        }
    }
    pub fn validate(self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 || self.right() > WIDTH || self.bottom() > HEIGHT {
            return Err("geometry must be nonempty and inside 1920x1080".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);
impl Color {
    pub fn parse(s: &str) -> Result<Self, String> {
        let hex = s
            .strip_prefix('#')
            .ok_or("color must be #RRGGBB or #RRGGBBAA")?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("color must be #RRGGBB or #RRGGBBAA".into());
        }
        let n = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
        Ok(if hex.len() == 6 {
            Self((n >> 16) as u8, (n >> 8) as u8, n as u8, 255)
        } else {
            Self((n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8)
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub background: String,
    pub text_color: String,
    pub accent: String,
    pub font_size: u16,
    pub padding: u16,
    pub align: Align,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            background: "#152338EE".into(),
            text_color: "#F1F5F9".into(),
            accent: "#38BDF8".into(),
            font_size: 30,
            padding: 24,
            align: Align::Left,
        }
    }
}
impl Style {
    pub fn validate(&self) -> Result<(), String> {
        Color::parse(&self.background)?;
        Color::parse(&self.text_color)?;
        Color::parse(&self.accent)?;
        if !(8..=128).contains(&self.font_size) {
            return Err("font_size must be 8..128".into());
        }
        if self.padding > 128 {
            return Err("padding must be 0..128".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeatherConfig {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default = "refresh_default")]
    pub refresh_seconds: u64,
}
fn refresh_default() -> u64 {
    900
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioConfig {
    pub name: String,
    pub stream_url: String,
    #[serde(default = "volume_default")]
    pub volume: f32,
}
fn volume_default() -> f32 {
    0.7
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "config", rename_all = "snake_case")]
pub enum Config {
    Weather(WeatherConfig),
    Webradio(RadioConfig),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ComponentWire")]
pub struct ComponentSpec {
    pub id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub style: Style,
    #[serde(flatten)]
    pub kind: Config,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentWire {
    id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    #[serde(default)]
    style: Style,
    #[serde(rename = "type")]
    kind: String,
    config: serde_json::Value,
}
impl TryFrom<ComponentWire> for ComponentSpec {
    type Error = String;
    fn try_from(w: ComponentWire) -> Result<Self, String> {
        let kind = serde_json::from_value(serde_json::json!({"type":w.kind,"config":w.config}))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            id: w.id,
            x: w.x,
            y: w.y,
            width: w.width,
            height: w.height,
            style: w.style,
            kind,
        })
    }
}
impl ComponentSpec {
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub schema_version: u32,
    pub components: Vec<ComponentSpec>,
}
impl Scene {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("schema_version must be 1".into());
        }
        if self.components.len() > MAX_COMPONENTS {
            return Err(format!("at most {MAX_COMPONENTS} components"));
        }
        let mut ids = HashSet::new();
        for c in &self.components {
            if c.id.is_empty()
                || c.id.len() > 64
                || !c
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err("id must contain 1..64 ASCII letters, digits, '_' or '-'".into());
            }
            if !ids.insert(&c.id) {
                return Err(format!("duplicate component id: {}", c.id));
            }
            c.rect().validate().map_err(|e| format!("{}: {e}", c.id))?;
            c.style.validate().map_err(|e| format!("{}: {e}", c.id))?;
            crate::component::validate(c).map_err(|e| format!("{}: {e}", c.id))?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub x: Option<u32>,
    pub y: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub style: Option<serde_json::Value>,
    pub config: Option<serde_json::Value>,
}
impl Patch {
    pub fn apply(self, c: &ComponentSpec) -> Result<ComponentSpec, String> {
        let mut next = c.clone();
        if let Some(x) = self.x {
            next.x = x
        }
        if let Some(y) = self.y {
            next.y = y
        }
        if let Some(w) = self.width {
            next.width = w
        }
        if let Some(h) = self.height {
            next.height = h
        }
        if let Some(s) = self.style {
            let mut v = serde_json::to_value(&next.style).unwrap();
            merge(&mut v, s)?;
            next.style = serde_json::from_value(v).map_err(|e| e.to_string())?;
        }
        if let Some(config) = self.config {
            let mut v = serde_json::to_value(&next.kind).unwrap();
            merge(&mut v["config"], config)?;
            next.kind = serde_json::from_value(v).map_err(|e| e.to_string())?;
        }
        Scene {
            schema_version: 1,
            components: vec![next.clone()],
        }
        .validate()?;
        Ok(next)
    }
}
fn merge(target: &mut serde_json::Value, patch: serde_json::Value) -> Result<(), String> {
    let p = patch.as_object().ok_or("patch must be an object")?;
    let t = target.as_object_mut().ok_or("target must be an object")?;
    for (k, v) in p {
        if !t.contains_key(k) {
            return Err(format!("unknown field: {k}"));
        }
        t.insert(k.clone(), v.clone());
    }
    Ok(())
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Play,
    Pause,
    Stop,
    Volume { value: f32 },
}
