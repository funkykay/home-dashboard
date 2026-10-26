use crate::{engine::Internal, model::WeatherConfig, retry::Backoff};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
pub struct Forecast {
    pub date: String,
    pub minimum: f64,
    pub maximum: f64,
    pub condition: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct WeatherData {
    pub temperature: f64,
    pub condition: String,
    pub forecast: Vec<Forecast>,
    pub observed_at: String,
}
#[derive(Deserialize)]
struct Response {
    current: Current,
    daily: Daily,
}
#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    weather_code: u16,
    time: String,
}
#[derive(Deserialize)]
struct Daily {
    time: Vec<String>,
    weather_code: Vec<u16>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
}
pub fn parse(bytes: &[u8]) -> Result<WeatherData> {
    let r: Response = serde_json::from_slice(bytes).context("invalid Open-Meteo JSON")?;
    let d = r.daily;
    if d.time.len() < 3
        || d.time.len() > 16
        || d.weather_code.len() != d.time.len()
        || d.temperature_2m_max.len() != d.time.len()
        || d.temperature_2m_min.len() != d.time.len()
        || !valid_temp(r.current.temperature_2m)
        || r.current.time.len() > 40
    {
        bail!("invalid weather data shape or temperature");
    }
    let mut forecast = Vec::with_capacity(3);
    for i in 0..3 {
        if !valid_temp(d.temperature_2m_min[i])
            || !valid_temp(d.temperature_2m_max[i])
            || d.time[i].len() != 10
        {
            bail!("invalid forecast");
        }
        forecast.push(Forecast {
            date: d.time[i].clone(),
            minimum: d.temperature_2m_min[i],
            maximum: d.temperature_2m_max[i],
            condition: condition(d.weather_code[i]).into(),
        });
    }
    Ok(WeatherData {
        temperature: r.current.temperature_2m,
        condition: condition(r.current.weather_code).into(),
        forecast,
        observed_at: r.current.time,
    })
}
fn valid_temp(v: f64) -> bool {
    v.is_finite() && (-100.0..=70.0).contains(&v)
}
pub fn condition(code: u16) -> &'static str {
    match code {
        0 => "Klar",
        1 => "Überwiegend klar",
        2 => "Teilweise bewölkt",
        3 => "Bedeckt",
        45 | 48 => "Nebel",
        51 | 53 | 55 => "Nieselregen",
        56 | 57 => "Gefrierender Nieselregen",
        61 | 63 | 65 => "Regen",
        66 | 67 => "Gefrierender Regen",
        71 | 73 | 75 | 77 => "Schnee",
        80..=82 => "Regenschauer",
        85 | 86 => "Schneeschauer",
        95 => "Gewitter",
        96 | 99 => "Gewitter mit Hagel",
        _ => "Unbekannt",
    }
}
type Cache = BTreeMap<(i64, i64), (Instant, Arc<WeatherData>)>;
pub struct WeatherService {
    client: reqwest::Client,
    endpoint: String,
    cache: Mutex<Cache>,
    next: Mutex<Instant>,
}
impl WeatherService {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            endpoint: "https://api.open-meteo.com/v1/forecast".into(),
            cache: Mutex::new(BTreeMap::new()),
            next: Mutex::new(Instant::now()),
        }
    }
    async fn fetch(&self, c: &WeatherConfig) -> Result<Arc<WeatherData>> {
        let key = (
            (c.latitude * 100000.0).round() as i64,
            (c.longitude * 100000.0).round() as i64,
        );
        // The lock coalesces equal-location requests, including in-flight requests.
        let mut cache = self.cache.lock().await;
        if let Some((at, data)) = cache.get(&key)
            && at.elapsed() < Duration::from_secs(c.refresh_seconds)
        {
            return Ok(data.clone());
        }
        let mut next = self.next.lock().await;
        tokio::time::sleep_until((*next).into()).await;
        *next = Instant::now() + Duration::from_secs(2);
        drop(next);
        let mut response = self
            .client
            .get(&self.endpoint)
            .query(&[
                ("latitude", c.latitude.to_string()),
                ("longitude", c.longitude.to_string()),
                ("current", "temperature_2m,weather_code".into()),
                (
                    "daily",
                    "weather_code,temperature_2m_max,temperature_2m_min".into(),
                ),
                ("forecast_days", "3".into()),
                ("timezone", "auto".into()),
            ])
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = Vec::with_capacity(8192);
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 131072 {
                bail!("weather response exceeds 128 KiB");
            }
            bytes.extend_from_slice(&chunk);
        }
        let data = Arc::new(parse(&bytes)?);
        if cache.len() >= 64
            && let Some(key) = cache.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| *k)
        {
            cache.remove(&key);
        }
        cache.insert(key, (Instant::now(), data.clone()));
        Ok(data)
    }
    pub fn spawn(
        self: Arc<Self>,
        id: String,
        generation: u64,
        config: WeatherConfig,
        events: mpsc::Sender<Internal>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut retry = Backoff::new(generation);
            loop {
                let result = tokio::select! { _=cancel.cancelled()=>break, result=self.fetch(&config)=>result };
                let (event, delay) = match result {
                    Ok(data) => {
                        retry.reset();
                        (
                            Internal::Weather {
                                id: id.clone(),
                                generation,
                                data: Ok(data),
                            },
                            Duration::from_secs(config.refresh_seconds),
                        )
                    }
                    Err(e) => (
                        Internal::Weather {
                            id: id.clone(),
                            generation,
                            data: Err(e.to_string()),
                        },
                        retry.delay(),
                    ),
                };
                tokio::select! {_=cancel.cancelled()=>break,result=events.send(event)=>{if result.is_err(){break}}}
                tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(delay)=>{}}
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_data_is_rejected() {
        assert!(parse(br#"{"current":{"temperature_2m":12,"weather_code":0,"time":"now"},"daily":{"time":[],"weather_code":[],"temperature_2m_min":[],"temperature_2m_max":[]}}"#).is_err());
    }
    #[test]
    fn german_conditions() {
        assert_eq!(condition(3), "Bedeckt");
        assert_eq!(condition(999), "Unbekannt");
    }
    #[tokio::test]
    async fn cache_coalesces_and_retry_worker_cleans_up() {
        use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route("/", get(move || {
            let count=count.clone(); async move {
                if count.fetch_add(1, Ordering::SeqCst)==0 { return (StatusCode::SERVICE_UNAVAILABLE,"temporary").into_response(); }
                axum::Json(serde_json::json!({"current":{"temperature_2m":12.5,"weather_code":3,"time":"2026-10-25T12:00"},"daily":{"time":["2026-10-25","2026-10-26","2026-10-27"],"weather_code":[3,0,61],"temperature_2m_min":[5,6,7],"temperature_2m_max":[12,13,14]}})).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut service = WeatherService::new(reqwest::Client::new());
        service.endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server_cancel = CancellationToken::new();
        let token = server_cancel.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(token.cancelled_owned())
                .await
                .unwrap();
        });
        let service = Arc::new(service);
        let config = WeatherConfig {
            latitude: 52.5,
            longitude: 13.4,
            refresh_seconds: 900,
        };
        let (tx, mut rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let task = service
            .clone()
            .spawn("weather".into(), 1, config.clone(), tx, cancel.clone());
        assert!(matches!(
            rx.recv().await,
            Some(Internal::Weather { data: Err(_), .. })
        ));
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, Internal::Weather { data: Ok(_), .. }));
        let (a, b) = tokio::join!(service.fetch(&config), service.fetch(&config));
        assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(rx.recv().await.is_none());
        server_cancel.cancel();
        server.await.unwrap();
    }
}
