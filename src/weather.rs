use crate::{engine::Internal, model::WeatherConfig, retry::Backoff};
use anyhow::{Context, Result, bail};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore, mpsc};
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
type Key = (i64, i64);
type FetchResult = Result<Arc<WeatherData>, Arc<str>>;
type Flight = Shared<BoxFuture<'static, FetchResult>>;
#[derive(Default)]
struct Cache {
    data: BTreeMap<Key, (Instant, Arc<WeatherData>)>,
    flights: BTreeMap<Key, Weak<Flight>>,
}
pub struct WeatherService {
    client: reqwest::Client,
    endpoint: String,
    cache: Mutex<Cache>,
    next: Arc<Mutex<Instant>>,
    slots: Arc<Semaphore>,
}
impl WeatherService {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            endpoint: "https://api.open-meteo.com/v1/forecast".into(),
            cache: Mutex::new(Cache::default()),
            next: Arc::new(Mutex::new(Instant::now())),
            slots: Arc::new(Semaphore::new(4)),
        }
    }
    async fn fetch(&self, c: &WeatherConfig) -> Result<Arc<WeatherData>> {
        let key = (
            (c.latitude * 100000.0).round() as i64,
            (c.longitude * 100000.0).round() as i64,
        );
        let flight = {
            let mut cache = self.cache.lock().await;
            if let Some((at, data)) = cache.data.get(&key)
                && at.elapsed() < Duration::from_secs(c.refresh_seconds)
            {
                return Ok(data.clone());
            }
            if let Some(flight) = cache.flights.get(&key).and_then(Weak::upgrade) {
                flight
            } else {
                // Weak entries do not keep cancelled requests (or their permits) alive.
                cache.flights.retain(|_, flight| flight.strong_count() > 0);
                if cache.flights.len() >= 64 {
                    bail!("too many weather locations in flight");
                }
                let client = self.client.clone();
                let endpoint = self.endpoint.clone();
                let next = self.next.clone();
                let slots = self.slots.clone();
                let config = c.clone();
                let flight = Arc::new(
                    async move {
                        let _permit = slots.acquire().await.expect("weather semaphore open");
                        // Deliberate global start-rate limit, independent of HTTP latency.
                        let mut next = next.lock().await;
                        tokio::time::sleep_until((*next).into()).await;
                        *next = Instant::now() + Duration::from_secs(2);
                        drop(next);
                        Self::request(client, endpoint, config)
                            .await
                            .map_err(|error| Arc::<str>::from(error.to_string()))
                    }
                    .boxed()
                    .shared(),
                );
                cache.flights.insert(key, Arc::downgrade(&flight));
                flight
            }
        };
        // All callers observe the same success/error; the last cancellation drops the I/O.
        let data = flight.as_ref().clone().await.map_err(anyhow::Error::msg)?;
        let mut cache = self.cache.lock().await;
        if !cache.data.contains_key(&key)
            && cache.data.len() >= 64
            && let Some(oldest) = cache.data.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| *k)
        {
            cache.data.remove(&oldest);
        }
        cache
            .data
            .entry(key)
            .and_modify(|entry| {
                if !Arc::ptr_eq(&entry.1, &data) {
                    *entry = (Instant::now(), data.clone());
                }
            })
            .or_insert_with(|| (Instant::now(), data.clone()));
        Ok(data)
    }
    async fn request(
        client: reqwest::Client,
        endpoint: String,
        c: WeatherConfig,
    ) -> Result<Arc<WeatherData>> {
        let mut response = client
            .get(&endpoint)
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
        Ok(Arc::new(parse(&bytes)?))
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
    fn fixture() -> axum::Json<serde_json::Value> {
        axum::Json(
            serde_json::json!({"current":{"temperature_2m":12.5,"weather_code":3,"time":"2026-10-25T12:00"},"daily":{"time":["2026-10-25","2026-10-26","2026-10-27"],"weather_code":[3,0,61],"temperature_2m_min":[5,6,7],"temperature_2m_max":[12,13,14]}}),
        )
    }
    #[tokio::test]
    async fn slow_location_does_not_block_others_and_cancellation_releases_flight() {
        use axum::{Router, extract::Query, routing::get};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let started = Arc::new(tokio::sync::Notify::new());
        let unblock = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (signal, gate, count) = (started.clone(), unblock.clone(), calls.clone());
        let app = Router::new().route(
            "/",
            get(move |Query(query): Query<BTreeMap<String, String>>| {
                let (signal, gate, count) = (signal.clone(), gate.clone(), count.clone());
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if query["latitude"] == "1" {
                        signal.notify_one();
                        gate.notified().await;
                    }
                    fixture()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut service = WeatherService::new(reqwest::Client::new());
        service.endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let service = Arc::new(service);
        let a = WeatherConfig {
            latitude: 1.0,
            longitude: 1.0,
            refresh_seconds: 300,
        };
        let b = WeatherConfig {
            latitude: 2.0,
            ..a.clone()
        };
        let s = service.clone();
        let config = a.clone();
        let first = tokio::spawn(async move { s.fetch(&config).await });
        started.notified().await;
        let s = service.clone();
        let same = tokio::spawn(async move { s.fetch(&a).await });
        // A is held indefinitely by the fixture. B only waits for the start-rate limit.
        let at = Instant::now();
        let (one, two) = tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(service.fetch(&b), service.fetch(&b))
        })
        .await
        .unwrap();
        assert!(at.elapsed() >= Duration::from_millis(1800));
        assert!(Arc::ptr_eq(&one.unwrap(), &two.unwrap()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(service.slots.available_permits(), 3);
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(service.slots.available_permits(), 3); // remaining A waiter owns I/O
        same.abort();
        assert!(same.await.unwrap_err().is_cancelled());
        assert_eq!(service.slots.available_permits(), 4);
        assert!(
            service
                .cache
                .lock()
                .await
                .flights
                .values()
                .all(|f| f.upgrade().is_none())
        );
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn concurrent_failures_coalesce_and_can_retry() {
        use axum::{Router, http::StatusCode, routing::get};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut service = WeatherService::new(reqwest::Client::new());
        service.endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = WeatherConfig {
            latitude: 1.0,
            longitude: 1.0,
            refresh_seconds: 300,
        };
        let (a, b) = tokio::join!(service.fetch(&config), service.fetch(&config));
        assert_eq!(a.unwrap_err().to_string(), b.unwrap_err().to_string());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(service.fetch(&config).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(service.cache.lock().await.data.is_empty());
        server.abort();
        let _ = server.await;
    }
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
