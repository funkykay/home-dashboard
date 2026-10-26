use crate::model::{HEIGHT, Rect, WIDTH};
use anyhow::Result;
#[cfg(target_os = "linux")]
mod linux;
pub trait Display: Send {
    fn present(&mut self, pixels: &[u32], dirty: &[Rect]) -> Result<()>;
    fn name(&self) -> &str;
}
pub struct Headless {
    pub presents: u64,
    pub output: Option<std::path::PathBuf>,
}
impl Display for Headless {
    fn name(&self) -> &str {
        "headless"
    }
    fn present(&mut self, pixels: &[u32], _: &[Rect]) -> Result<()> {
        self.presents += 1;
        if let Some(path) = &self.output {
            use std::io::Write;
            let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
            write!(out, "P6\n{WIDTH} {HEIGHT}\n255\n")?;
            for p in pixels {
                out.write_all(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8])?;
            }
            out.flush()?;
        }
        Ok(())
    }
}
pub fn open(backend: &str) -> Result<Box<dyn Display>> {
    if backend == "headless" {
        return Ok(Box::new(Headless {
            presents: 0,
            output: std::env::var_os("DASHBOARD_SNAPSHOT").map(Into::into),
        }));
    }
    #[cfg(target_os = "linux")]
    {
        linux::open(backend)
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("hardware display requires Linux; use --headless for development")
    }
}
