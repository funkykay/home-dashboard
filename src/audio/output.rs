use super::Block;
use anyhow::Result;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub trait Output: Send {
    fn write(&mut self, samples: &[i16], cancel: &CancellationToken) -> Result<()>;
}
pub trait OutputFactory: Send + Sync {
    fn open(&self, rate: u32, channels: u32) -> Result<Box<dyn Output>>;
}
pub struct NullFactory;
struct NullOutput {
    rate: u32,
    channels: u32,
}
impl OutputFactory for NullFactory {
    fn open(&self, rate: u32, channels: u32) -> Result<Box<dyn Output>> {
        Ok(Box::new(NullOutput { rate, channels }))
    }
}
impl Output for NullOutput {
    fn write(&mut self, samples: &[i16], cancel: &CancellationToken) -> Result<()> {
        let deadline = Instant::now()
            + Duration::from_secs_f64(samples.len() as f64 / f64::from(self.rate * self.channels));
        while Instant::now() < deadline && !cancel.is_cancelled() {
            std::thread::sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        Ok(())
    }
}
pub fn factory(headless: bool, device: String) -> Result<Arc<dyn OutputFactory>> {
    if headless {
        return Ok(Arc::new(NullFactory));
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Arc::new(AlsaFactory(device)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        anyhow::bail!("ALSA audio requires Linux; use --headless")
    }
}
pub fn run(
    factory: Arc<dyn OutputFactory>,
    input: crossbeam_channel::Receiver<Block>,
    pool: crossbeam_channel::Sender<Vec<i16>>,
    volume: Arc<AtomicU32>,
    cancel: CancellationToken,
    playing: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    let mut output: Option<(u32, u32, Box<dyn Output>)> = None;
    while !cancel.is_cancelled() {
        let mut block = match input.recv_timeout(Duration::from_millis(100)) {
            Ok(b) => b,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                playing.send_if_modified(|v| {
                    let changed = *v;
                    *v = false;
                    changed
                });
                continue;
            }
            Err(_) => break,
        };
        if output
            .as_ref()
            .is_none_or(|(r, c, _)| *r != block.rate || *c != block.channels)
        {
            // Close the previous PCM before opening a different format on the same device.
            output.take();
            output = Some((
                block.rate,
                block.channels,
                factory.open(block.rate, block.channels)?,
            ));
        }
        let gain = f32::from_bits(volume.load(Ordering::Relaxed));
        if gain != 1.0 {
            for sample in &mut block.samples {
                *sample = (*sample as f32 * gain) as i16;
            }
        }
        output.as_mut().unwrap().2.write(&block.samples, &cancel)?;
        playing.send_if_modified(|v| {
            let changed = !*v;
            *v = true;
            changed
        });
        let _ = pool.try_send(block.samples);
    }
    Ok(())
}
#[cfg(target_os = "linux")]
struct AlsaFactory(String);
#[cfg(target_os = "linux")]
struct AlsaOutput {
    pcm: alsa::pcm::PCM,
    channels: usize,
}
#[cfg(target_os = "linux")]
impl OutputFactory for AlsaFactory {
    fn open(&self, rate: u32, channels: u32) -> Result<Box<dyn Output>> {
        use alsa::{
            Direction, ValueOr,
            pcm::{Access, Format, HwParams, PCM},
        };
        let pcm = PCM::new(&self.0, Direction::Playback, true)?;
        {
            let h = HwParams::any(&pcm)?;
            h.set_access(Access::RWInterleaved)?;
            h.set_format(Format::s16())?;
            h.set_channels(channels)?;
            h.set_rate(rate, ValueOr::Nearest)?;
            h.set_buffer_time_near(200000, ValueOr::Nearest)?;
            h.set_period_time_near(20000, ValueOr::Nearest)?;
            pcm.hw_params(&h)?;
            if h.get_rate() != Ok(rate) {
                anyhow::bail!("ALSA rate mismatch; use a plughw HDMI device");
            }
            let sw = pcm.sw_params_current()?;
            sw.set_start_threshold(h.get_buffer_size()? / 2)?;
            pcm.sw_params(&sw)?;
        }
        Ok(Box::new(AlsaOutput {
            pcm,
            channels: channels as usize,
        }))
    }
}
#[cfg(target_os = "linux")]
impl Output for AlsaOutput {
    fn write(&mut self, samples: &[i16], cancel: &CancellationToken) -> Result<()> {
        let io = self.pcm.io_i16()?;
        let mut offset = 0;
        let mut progress = Instant::now();
        while offset < samples.len() && !cancel.is_cancelled() {
            match io.writei(&samples[offset..]) {
                Ok(n) if n > 0 => {
                    offset += n * self.channels;
                    progress = Instant::now();
                }
                Ok(_) => {
                    self.pcm.wait(Some(50))?;
                }
                Err(e) if e.errno() == libc::EAGAIN => {
                    self.pcm.wait(Some(50))?;
                }
                Err(e) if e.errno() == libc::EPIPE => {
                    self.pcm.prepare()?;
                }
                Err(e) => return Err(e.into()),
            }
            if progress.elapsed() > Duration::from_secs(3) {
                anyhow::bail!("ALSA made no playback progress for 3 seconds");
            }
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Drop for AlsaOutput {
    fn drop(&mut self) {
        let _ = self.pcm.drop();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn real_alsa_backend_configures_and_writes_to_null_pcm() {
        let factory = factory(false, "null".into()).unwrap();
        let cancel = CancellationToken::new();
        for (rate, channels) in [(48000, 2), (44100, 1)] {
            let mut output = factory.open(rate, channels).unwrap();
            output.write(&[0; 2048], &cancel).unwrap();
            drop(output);
        }
    }
}
