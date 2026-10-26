mod icy;
pub mod output;
mod ring;
use crate::{engine::Internal, model::RadioConfig, retry::Backoff};
use anyhow::{Context, Result, bail};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::FormatOptions,
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::{Limit, MetadataOptions},
    probe::Hint,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub struct Block {
    pub samples: Vec<i16>,
    pub rate: u32,
    pub channels: u32,
}
struct Session {
    cancel: CancellationToken,
    task: JoinHandle<Result<()>>,
    volume: Arc<AtomicU32>,
}
pub struct AudioManager {
    pub owner: Option<String>,
    pub session: u64,
    active: Option<Session>,
    pub releasing: bool,
    pub cleanup_error: Option<String>,
    pub quarantined: bool,
    deadline: Option<tokio::time::Instant>,
    factory: Arc<dyn output::OutputFactory>,
    client: reqwest::Client,
    events: mpsc::Sender<Internal>,
}
impl AudioManager {
    pub fn new(
        factory: Arc<dyn output::OutputFactory>,
        client: reqwest::Client,
        events: mpsc::Sender<Internal>,
    ) -> Self {
        Self {
            owner: None,
            session: 0,
            active: None,
            releasing: false,
            cleanup_error: None,
            quarantined: false,
            deadline: None,
            factory,
            client,
            events,
        }
    }
    pub fn begin_release(&mut self) {
        if self.releasing {
            return;
        }
        self.session = self.session.wrapping_add(1);
        if let Some(s) = &self.active {
            s.cancel.cancel();
            self.releasing = true;
            self.cleanup_error = None;
            self.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
        }
    }
    pub fn cleanup_deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }
    pub fn cleanup_timed_out(&mut self) {
        self.deadline = None;
        self.cleanup_error =
            Some("audio cleanup exceeded five seconds; output is still reserved".into());
        tracing::error!(
            "audio cleanup exceeded five seconds; retaining workers and output reservation"
        );
    }
    /// Cancel-safe: the actor polls the stored handle alongside incoming requests.
    /// Never abort a supervisor: its blocking workers must all be joined.
    pub async fn finish_release(&mut self) {
        if !self.releasing {
            std::future::pending::<()>().await;
        }
        let result = (&mut self.active.as_mut().unwrap().task).await;
        self.active = None;
        self.releasing = false;
        self.deadline = None;
        self.cleanup_error = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => {
                // A supervisor panic cannot prove that its child workers joined.
                self.quarantined = true;
                Some(format!(
                    "audio supervisor failed; output quarantined: {error}"
                ))
            }
        };
        if let Some(error) = &self.cleanup_error {
            tracing::error!(%error, "audio cleanup failed");
        }
    }
    /// Shutdown/public API waits for actual resource release, with a visible watchdog.
    pub async fn release(&mut self) -> Option<String> {
        self.begin_release();
        if self.releasing
            && tokio::time::timeout(Duration::from_secs(5), self.finish_release())
                .await
                .is_err()
        {
            self.cleanup_timed_out();
            self.finish_release().await;
        }
        if self.quarantined {
            None
        } else {
            self.owner.take()
        }
    }
    pub async fn claim(&mut self, id: String, generation: u64, config: RadioConfig) {
        self.release().await;
        if self.quarantined {
            return;
        }
        self.cleanup_error = None;
        let cancel = CancellationToken::new();
        let volume = Arc::new(AtomicU32::new(config.volume.to_bits()));
        let ctx = ContextAudio {
            id: id.clone(),
            generation,
            session: self.session,
            events: self.events.clone(),
            client: self.client.clone(),
            factory: self.factory.clone(),
            volume: volume.clone(),
        };
        let token = cancel.clone();
        let task = tokio::spawn(async move { supervise(ctx, config, token).await });
        self.owner = Some(id);
        self.active = Some(Session {
            cancel,
            task,
            volume,
        });
    }
    pub fn volume(&self, value: f32) {
        if let Some(s) = &self.active {
            s.volume.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}
impl Drop for AudioManager {
    fn drop(&mut self) {
        if let Some(s) = &self.active {
            s.cancel.cancel();
        }
    }
}
struct ContextAudio {
    id: String,
    generation: u64,
    session: u64,
    events: mpsc::Sender<Internal>,
    client: reqwest::Client,
    factory: Arc<dyn output::OutputFactory>,
    volume: Arc<AtomicU32>,
}
impl ContextAudio {
    async fn state(&self, state: &str, error: Option<String>, cancel: &CancellationToken) {
        let event = Internal::Playback {
            id: self.id.clone(),
            generation: self.generation,
            session: self.session,
            state: state.into(),
            error,
        };
        tokio::select! {_=cancel.cancelled()=>{},_=self.events.send(event)=>{}}
    }
}
async fn supervise(
    ctx: ContextAudio,
    config: RadioConfig,
    cancel: CancellationToken,
) -> Result<()> {
    let mut retry = Backoff::new(ctx.generation ^ ctx.session.wrapping_mul(7919));
    loop {
        if cancel.is_cancelled() {
            break;
        }
        ctx.state("buffering", None, &cancel).await;
        let started = std::time::Instant::now();
        let result = attempt(&ctx, &config, &cancel).await;
        if cancel.is_cancelled() {
            return result;
        }
        if started.elapsed() > Duration::from_secs(30) {
            retry.reset();
        }
        let error = result
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "radio stream ended".into());
        ctx.state("error", Some(error), &cancel).await;
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(retry.delay())=>{}}
    }
    Ok(())
}
async fn attempt(
    ctx: &ContextAudio,
    config: &RadioConfig,
    parent: &CancellationToken,
) -> Result<()> {
    let cancel = parent.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let ring = ring::Ring::new(256 * 1024, cancel.clone());
    let (metadata_tx, mut metadata_rx) = watch::channel(String::new());
    let (playing_tx, mut playing_rx) = watch::channel(false);
    let r = ring.clone();
    let client = ctx.client.clone();
    let url = config.stream_url.clone();
    let net_cancel = cancel.clone();
    let mut network = tokio::spawn(async move {
        let result = tokio::select! {_=net_cancel.cancelled()=>Ok(()),r=download(client,url,r.clone(),metadata_tx)=>r};
        r.close();
        result
    });
    let (pcm_tx, pcm_rx) = crossbeam_channel::bounded(8);
    let (pool_tx, pool_rx) = crossbeam_channel::bounded(8);
    for _ in 0..8 {
        pool_tx.send(Vec::with_capacity(16384)).unwrap();
    }
    let r = ring.clone();
    let decoder_cancel = cancel.clone();
    let mut decoder =
        tokio::task::spawn_blocking(move || decode(r, pcm_tx, pool_rx, decoder_cancel));
    let factory = ctx.factory.clone();
    let volume = ctx.volume.clone();
    let output_cancel = cancel.clone();
    let mut output = tokio::task::spawn_blocking(move || {
        output::run(factory, pcm_rx, pool_tx, volume, output_cancel, playing_tx)
    });
    enum Finished {
        Network,
        Decoder,
        Output,
        Cancel,
    }
    let (finished, result, panicked) = loop {
        tokio::select! {
            _=parent.cancelled()=>break(Finished::Cancel,Ok(()),false),
            r=&mut network=>{let panicked=r.is_err();break(Finished::Network,r.context("network task panicked").and_then(|x|x),panicked)},
            r=&mut decoder=>{let panicked=r.is_err();break(Finished::Decoder,r.context("decoder task panicked").and_then(|x|x),panicked)},
            r=&mut output=>{let panicked=r.is_err();break(Finished::Output,r.context("audio output task panicked").and_then(|x|x),panicked)},
            r=playing_rx.changed()=>{if r.is_ok(){let playing=*playing_rx.borrow_and_update();ctx.state(if playing{"playing"}else{"buffering"},None,parent).await;}},
            r=metadata_rx.changed()=>{if r.is_ok(){let title=metadata_rx.borrow_and_update().clone();let event=Internal::Metadata{id:ctx.id.clone(),generation:ctx.generation,session:ctx.session,title};tokio::select!{_=parent.cancelled()=>{},_=ctx.events.send(event)=>{}}}},
        }
    };
    cancel.cancel();
    ring.close();
    if !matches!(finished, Finished::Network) {
        let _ = network.await;
    }
    // Always join both blocking workers, even if either reports an error.
    let decoded = if !matches!(finished, Finished::Decoder) {
        match decoder.await {
            Ok(result) if !parent.is_cancelled() => result,
            Ok(_) => Ok(()), // closing the input may interrupt probing/decoding
            Err(error) => Err(error).context("decoder cleanup panicked"),
        }
    } else {
        Ok(())
    };
    let played = if !matches!(finished, Finished::Output) {
        output
            .await
            .context("output cleanup panicked")
            .and_then(|x| x)
    } else {
        Ok(())
    };
    if parent.is_cancelled() {
        played?;
        decoded?;
        // Ring closure can race the parent's cancellation branch during probing.
        // Preserve task panics and output failures, not the expected input interruption.
        return if !panicked && matches!(finished, Finished::Network | Finished::Decoder) {
            Ok(())
        } else {
            result
        };
    }
    // Cancellation of the sibling workers is a consequence, not the cause.
    // Preserve the original failure (in particular an ALSA open/write error).
    result?;
    played?;
    decoded
}
async fn download(
    client: reqwest::Client,
    url: String,
    ring: Arc<ring::Ring>,
    metadata: watch::Sender<String>,
) -> Result<()> {
    let mut response = client
        .get(url)
        .header("Icy-MetaData", "1")
        .send()
        .await?
        .error_for_status()?;
    let interval = response
        .headers()
        .get("icy-metaint")
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|n| *n > 0 && *n <= 16 * 1024 * 1024)
                .context("invalid icy-metaint")
        })
        .transpose()?
        .unwrap_or(0);
    let mut icy = icy::Icy::new(interval);
    while let Some(chunk) = response.chunk().await? {
        let mut bytes = chunk.as_ref();
        while !bytes.is_empty() {
            let (audio, title) = icy.consume(&mut bytes);
            if let Some(audio) = audio {
                ring.write(audio).await?;
            }
            if let Some(title) = title {
                metadata.send_replace(title);
            }
        }
    }
    bail!("radio server closed the stream")
}
fn decode(
    ring: Arc<ring::Ring>,
    output: crossbeam_channel::Sender<Block>,
    pool: crossbeam_channel::Receiver<Vec<i16>>,
    cancel: CancellationToken,
) -> Result<()> {
    let source = MediaSourceStream::new(
        Box::new(ring::Source(ring)),
        MediaSourceStreamOptions {
            buffer_len: 64 * 1024,
        },
    );
    let probed = symphonia::default::get_probe().format(
        &Hint::new(),
        source,
        &FormatOptions::default(),
        &MetadataOptions {
            limit_metadata_bytes: Limit::Maximum(65536),
            limit_visual_bytes: Limit::Maximum(0),
        },
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .context("stream has no default audio track")?;
    let track_id = track.id;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut sample_buffer: Option<SampleBuffer<i16>> = None;
    let mut errors = 0u32;
    while !cancel.is_cancelled() {
        let packet = format.next_packet()?;
        format.metadata().skip_to_latest();
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => {
                errors = 0;
                d
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) if errors < 20 => {
                errors += 1;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        if !(1..=2).contains(&channels)
            || !(8000..=96000).contains(&spec.rate)
            || decoded.capacity() * channels > 16384
        {
            bail!("radio must be mono/stereo, 8–96 kHz, bounded audio packets");
        }
        let need = decoded.capacity() * channels;
        if sample_buffer.as_ref().is_none_or(|b| b.capacity() < need) {
            sample_buffer = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let samples = sample_buffer.as_mut().unwrap();
        samples.copy_interleaved_ref(decoded);
        let mut pcm = loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            match pool.recv_timeout(Duration::from_millis(100)) {
                Ok(v) => break v,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(_) => return Ok(()),
            }
        };
        pcm.clear();
        pcm.extend_from_slice(samples.samples());
        let mut block = Block {
            samples: pcm,
            rate: spec.rate,
            channels: channels as u32,
        };
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            match output.send_timeout(block, Duration::from_millis(100)) {
                Ok(()) => break,
                Err(crossbeam_channel::SendTimeoutError::Timeout(b)) => block = b,
                Err(_) => return Ok(()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn supervisor_panic_quarantines_output_without_claiming_release() {
        let (events, _rx) = mpsc::channel(1);
        let mut manager = AudioManager::new(
            Arc::new(output::NullFactory),
            reqwest::Client::new(),
            events,
        );
        manager.owner = Some("old".into());
        manager.active = Some(Session {
            cancel: CancellationToken::new(),
            task: tokio::spawn(async {
                panic!("supervisor panic");
            }),
            volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        });
        assert!(manager.release().await.is_none());
        assert!(manager.quarantined);
        assert_eq!(manager.owner.as_deref(), Some("old"));
        assert!(
            manager
                .cleanup_error
                .as_ref()
                .unwrap()
                .contains("quarantined")
        );
        manager
            .claim(
                "new".into(),
                1,
                RadioConfig {
                    name: "new".into(),
                    stream_url: "http://127.0.0.1:9/".into(),
                    volume: 1.0,
                },
            )
            .await;
        assert_eq!(manager.owner.as_deref(), Some("old"));
        assert!(manager.active.is_none());
    }
}
