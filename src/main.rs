use anyhow::{Context, Result};
use pi_dashboard::{
    api, audio, display,
    engine::Engine,
    render::{self, RenderThread, Renderer},
};
use std::{
    future::IntoFuture,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pi_dashboard=info".into()),
        )
        .init();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help") {
        println!(
            "pi-dashboard [--headless]\nEnvironment: DASHBOARD_BIND=0.0.0.0:8080 DASHBOARD_DISPLAY=auto\nDASHBOARD_FONT=/path/font.ttf DASHBOARD_ALSA=plughw:CARD=vc4hdmi,DEV=0\nDASHBOARD_DRM_DEVICE=/dev/dri/card1 DASHBOARD_CONNECTOR=HDMI-A-1\nDASHBOARD_TTY=/dev/tty1 DASHBOARD_SNAPSHOT=/path/frame.ppm (headless)"
        );
        return Ok(());
    }
    anyhow::ensure!(
        args.iter().all(|a| a == "--headless"),
        "unknown option; use --help"
    );
    let headless = args.iter().any(|a| a == "--headless");
    let backend = if headless {
        "headless".into()
    } else {
        std::env::var("DASHBOARD_DISPLAY").unwrap_or_else(|_| "auto".into())
    };
    let device = display::open(&backend)?;
    let name = device.name().to_string();
    let renderer = Renderer::new(render::load_font(
        std::env::var("DASHBOARD_FONT").ok().as_deref(),
    )?)?;
    let render = RenderThread::spawn(device, renderer)?;
    let factory = audio::output::factory(
        headless,
        std::env::var("DASHBOARD_ALSA").unwrap_or_else(|_| "plughw:CARD=vc4hdmi,DEV=0".into()),
    )?;
    let (engine, task) = Engine::spawn(Some(render.handle.clone()), factory, name);
    let bind = std::env::var("DASHBOARD_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind,"dashboard ready");
    let server = axum::serve(
        LimitedListener {
            listener,
            slots: Arc::new(Semaphore::new(64)),
        },
        api::router(engine.clone()),
    )
    .with_graceful_shutdown(engine.cancellation().cancelled_owned())
    .into_future();
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result,
        _ = shutdown() => {
            engine.shutdown();
            // A slow HTTP request must not keep hardware resources alive indefinitely.
            match tokio::time::timeout(std::time::Duration::from_secs(5), &mut server).await {
                Ok(result) => result,
                Err(_) => { tracing::warn!("HTTP drain exceeded five seconds"); Ok(()) }
            }
        }
    };
    engine.shutdown();
    task.await?;
    drop(engine);
    render.shutdown();
    result?;
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
struct LimitedListener {
    listener: TcpListener,
    slots: Arc<Semaphore>,
}
struct Connection {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
}
impl axum::serve::Listener for LimitedListener {
    type Io = Connection;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = self
                .slots
                .clone()
                .acquire_owned()
                .await
                .expect("connection semaphore");
            match self.listener.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    return (
                        Connection {
                            stream,
                            _permit: permit,
                        },
                        addr,
                    );
                }
                Err(e) => {
                    tracing::warn!(error=%e,"accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}
impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
