use super::Display;
use crate::model::{HEIGHT, Rect, WIDTH};
use anyhow::{Context, Result, bail};
use drm::{
    Device as _,
    buffer::{Buffer, DrmFourcc},
    control::{Device as _, connector, crtc, dumbbuffer::DumbBuffer, framebuffer},
};
use memmap2::MmapMut;
use std::{
    fs::{File, OpenOptions},
    os::fd::{AsFd, AsRawFd, BorrowedFd},
};

pub fn open(backend: &str) -> Result<Box<dyn Display>> {
    let console = Console::open()?;
    let display: Box<dyn Display> = match backend {
        "auto" => match Fb::open() {
            Ok(fb) => Box::new(fb),
            Err(e) => {
                tracing::warn!(error=%e,"fb0 unsuitable; trying DRM/KMS");
                Box::new(Kms::discover()?)
            }
        },
        "fb" => Box::new(Fb::open()?),
        "drm" => Box::new(Kms::discover()?),
        _ => bail!("DASHBOARD_DISPLAY must be auto, fb, drm or headless"),
    };
    Ok(Box::new(ConsoleDisplay {
        display,
        _console: console,
    }))
}
struct ConsoleDisplay {
    display: Box<dyn Display>,
    _console: Console,
}
impl Display for ConsoleDisplay {
    fn name(&self) -> &str {
        self.display.name()
    }
    fn present(&mut self, p: &[u32], r: &[Rect]) -> Result<()> {
        self.display.present(p, r)
    }
}
struct Console {
    file: File,
    previous: libc::c_int,
}
impl Console {
    fn open() -> Result<Self> {
        let path = std::env::var("DASHBOARD_TTY").unwrap_or_else(|_| "/dev/tty1".into());
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {path}; configure tty permissions"))?;
        let mut previous = 0;
        // SAFETY: KDGETMODE writes one valid int; file is a Linux virtual console.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x4B3B, &mut previous) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: KDSETMODE takes the integer mode, not a pointer.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x4B3A, 1) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { file, previous })
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), 0x4B3A, self.previous);
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}
#[repr(C)]
#[derive(Default)]
struct Var {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}
#[repr(C)]
#[derive(Default)]
struct Fix {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}
struct Fb {
    _file: File,
    map: MmapMut,
    stride: usize,
    start: usize,
    bpp: usize,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    alpha: Bitfield,
}
impl Fb {
    fn open() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fb0")
            .context("opening /dev/fb0")?;
        let mut v = Var::default();
        let mut f = Fix::default();
        // SAFETY: repr(C) layouts match linux/fb.h; kernel writes exactly these structures.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x4600, &mut v) } < 0
            || unsafe { libc::ioctl(file.as_raw_fd(), 0x4602, &mut f) } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if v.xres != WIDTH
            || v.yres != HEIGHT
            || !(v.bits_per_pixel == 16 || v.bits_per_pixel == 32)
            || f.type_ != 0
            || f.visual != 2
            || v.nonstd != 0
            || v.grayscale != 0
            || v.rotate != 0
            || v.vmode & 255 != 0
        {
            bail!(
                "fb0 must be progressive 1920x1080, packed truecolor RGB16/32; got {}x{} {}bpp visual={} type={}",
                v.xres,
                v.yres,
                v.bits_per_pixel,
                f.visual,
                f.type_
            );
        }
        let mut mask = 0u64;
        for b in [v.red, v.green, v.blue, v.transp] {
            if b.length == 0 {
                continue;
            }
            if b.msb_right != 0 || b.length > 8 || b.offset + b.length > v.bits_per_pixel {
                bail!("unsupported framebuffer bitfield");
            }
            let bits = ((1u64 << b.length) - 1) << b.offset;
            if mask & bits != 0 {
                bail!("overlapping framebuffer channels");
            }
            mask |= bits;
        }
        if [v.red, v.green, v.blue].iter().any(|b| b.length == 0) {
            bail!("missing RGB channels");
        }
        let bpp = (v.bits_per_pixel / 8) as usize;
        let stride = f.line_length as usize;
        let start = v.yoffset as usize * stride + v.xoffset as usize * bpp;
        if v.xoffset
            .checked_add(WIDTH)
            .is_none_or(|x| x > v.xres_virtual)
            || v.yoffset
                .checked_add(HEIGHT)
                .is_none_or(|y| y > v.yres_virtual)
            || stride < (v.xoffset as usize + WIDTH as usize) * bpp
            || start + (HEIGHT as usize - 1) * stride + WIDTH as usize * bpp > f.smem_len as usize
        {
            bail!("framebuffer stride/offset/memory inconsistent");
        }
        // SAFETY: checked bounds, exclusive writer through this object; kernel owns stable FB memory.
        let map = unsafe {
            memmap2::MmapOptions::new()
                .len(f.smem_len as usize)
                .map_mut(&file)
        }?;
        tracing::info!(stride,bpp,driver=%String::from_utf8_lossy(&f.id),"framebuffer accepted");
        Ok(Self {
            _file: file,
            map,
            stride,
            start,
            bpp,
            red: v.red,
            green: v.green,
            blue: v.blue,
            alpha: v.transp,
        })
    }
}
fn pack(v: u32, b: Bitfield) -> u32 {
    if b.length == 0 {
        0
    } else {
        ((v * ((1 << b.length) - 1) + 127) / 255) << b.offset
    }
}
impl Display for Fb {
    fn name(&self) -> &str {
        "fb0"
    }
    fn present(&mut self, p: &[u32], regions: &[Rect]) -> Result<()> {
        for r in regions {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let c = p[(y * WIDTH + x) as usize];
                    let pixel = pack((c >> 16) & 255, self.red)
                        | pack((c >> 8) & 255, self.green)
                        | pack(c & 255, self.blue)
                        | pack(255, self.alpha);
                    let at = self.start + y as usize * self.stride + x as usize * self.bpp;
                    self.map[at..at + self.bpp].copy_from_slice(&pixel.to_ne_bytes()[..self.bpp]);
                }
            }
        }
        Ok(())
    }
}
struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl drm::control::Device for Card {}
struct Kms {
    card: Card,
    buffer: DumbBuffer,
    fb: framebuffer::Handle,
    old: crtc::Info,
    old_connectors: Vec<connector::Handle>,
    name: String,
}
impl Kms {
    fn discover() -> Result<Self> {
        let paths = if let Ok(path) = std::env::var("DASHBOARD_DRM_DEVICE") {
            vec![path]
        } else {
            (0..8).map(|n| format!("/dev/dri/card{n}")).collect()
        };
        let mut errors = Vec::new();
        for path in paths {
            match Self::open(&path) {
                Ok(k) => return Ok(k),
                Err(e) => errors.push(format!("{path}: {e}")),
            }
        }
        bail!("no usable DRM device: {}", errors.join("; "))
    }
    fn open(path: &str) -> Result<Self> {
        let card = Card(OpenOptions::new().read(true).write(true).open(path)?);
        card.acquire_master_lock()
            .context("DRM master unavailable")?;
        let res = card.resource_handles()?;
        let requested = std::env::var("DASHBOARD_CONNECTOR").ok();
        let con = res
            .connectors()
            .iter()
            .filter_map(|h| card.get_connector(*h, true).ok())
            .find(|c| {
                c.state() == connector::State::Connected
                    && requested.as_ref().is_none_or(|s| {
                        s == &format!("{}-{}", c.interface().as_str(), c.interface_id())
                    })
            })
            .context("no connected connector")?;
        let mode = *con
            .modes()
            .iter()
            .filter(|m| {
                m.size() == (WIDTH as u16, HEIGHT as u16)
                    && !m.flags().contains(drm::control::ModeFlags::INTERLACE)
            })
            .max_by_key(|m| m.vrefresh())
            .context("connector has no progressive Full-HD mode")?;
        let crtc = con
            .encoders()
            .iter()
            .filter_map(|h| card.get_encoder(*h).ok())
            .find_map(|e| {
                e.crtc()
                    .or_else(|| res.filter_crtcs(e.possible_crtcs()).first().copied())
            })
            .context("no compatible CRTC")?;
        let old = card.get_crtc(crtc)?;
        let old_connectors = res
            .connectors()
            .iter()
            .copied()
            .filter(|h| {
                card.get_connector(*h, false)
                    .ok()
                    .and_then(|c| c.current_encoder())
                    .and_then(|h| card.get_encoder(h).ok())
                    .and_then(|e| e.crtc())
                    == Some(crtc)
            })
            .collect();
        let mut buffer = card.create_dumb_buffer((WIDTH, HEIGHT), DrmFourcc::Xrgb8888, 32)?;
        let fb = match card.add_framebuffer(&buffer, 24, 32) {
            Ok(f) => f,
            Err(e) => {
                let _ = card.destroy_dumb_buffer(buffer);
                return Err(e.into());
            }
        };
        let result = (|| -> Result<()> {
            card.map_dumb_buffer(&mut buffer)?.as_mut().fill(0);
            card.set_crtc(crtc, Some(fb), (0, 0), &[con.handle()], Some(mode))?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = card.destroy_framebuffer(fb);
            let _ = card.destroy_dumb_buffer(buffer);
            return Err(e);
        }
        tracing::info!(device=path,mode=?mode,"DRM/KMS active");
        Ok(Self {
            card,
            buffer,
            fb,
            old,
            old_connectors,
            name: format!("drm:{path}"),
        })
    }
}
impl Display for Kms {
    fn name(&self) -> &str {
        &self.name
    }
    fn present(&mut self, p: &[u32], regions: &[Rect]) -> Result<()> {
        let pitch = self.buffer.pitch() as usize;
        let mut map = self.card.map_dumb_buffer(&mut self.buffer)?;
        for r in regions {
            for y in r.y..r.bottom() {
                let start = y as usize * pitch + r.x as usize * 4;
                let row = &p[(y * WIDTH + r.x) as usize..(y * WIDTH + r.right()) as usize];
                // SAFETY: u32 slice is initialized and byte view has exactly its allocation length.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        row.as_ptr().cast::<u8>(),
                        std::mem::size_of_val(row),
                    )
                };
                map[start..start + bytes.len()].copy_from_slice(bytes);
            }
        }
        Ok(())
    }
}
impl Drop for Kms {
    fn drop(&mut self) {
        let _ = self.card.set_crtc(
            self.old.handle(),
            self.old.framebuffer(),
            self.old.position(),
            &self.old_connectors,
            self.old.mode(),
        );
        let _ = self.card.destroy_framebuffer(self.fb);
        let _ = self.card.destroy_dumb_buffer(self.buffer);
        let _ = self.card.release_master_lock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framebuffer_uapi_layout_and_rgb565_conversion() {
        assert_eq!(std::mem::size_of::<Var>(), 160);
        assert_eq!(std::mem::size_of::<Fix>(), 80);
        assert_eq!(std::mem::offset_of!(Fix, line_length), 48);
        assert_eq!(std::mem::offset_of!(Fix, mmio_start), 56);
        assert_eq!(
            pack(
                255,
                Bitfield {
                    offset: 11,
                    length: 5,
                    msb_right: 0
                }
            ),
            0xf800
        );
        assert_eq!(
            pack(
                128,
                Bitfield {
                    offset: 5,
                    length: 6,
                    msb_right: 0
                }
            ),
            0x0400
        );
    }
}
