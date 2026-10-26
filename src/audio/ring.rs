use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use symphonia::core::io::MediaSource;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
struct State {
    data: Box<[u8]>,
    head: usize,
    len: usize,
    closed: bool,
}
pub struct Ring {
    state: Mutex<State>,
    ready: Condvar,
    space: Notify,
    cancel: CancellationToken,
}
impl Ring {
    pub fn new(capacity: usize, cancel: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                data: vec![0; capacity].into_boxed_slice(),
                head: 0,
                len: 0,
                closed: false,
            }),
            ready: Condvar::new(),
            space: Notify::new(),
            cancel,
        })
    }
    pub async fn write(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let notified = self.space.notified();
            let n = {
                let mut s = self.state.lock().unwrap();
                if s.closed || self.cancel.is_cancelled() {
                    return Err(io::ErrorKind::ConnectionAborted.into());
                }
                let n = bytes.len().min(s.data.len() - s.len);
                let tail = (s.head + s.len) % s.data.len();
                let first = n.min(s.data.len() - tail);
                s.data[tail..tail + first].copy_from_slice(&bytes[..first]);
                s.data[..n - first].copy_from_slice(&bytes[first..n]);
                s.len += n;
                n
            };
            if n > 0 {
                bytes = &bytes[n..];
                self.ready.notify_one();
            } else {
                tokio::select! {_=self.cancel.cancelled()=>return Err(io::ErrorKind::ConnectionAborted.into()),_=notified=>{}}
            }
        }
        Ok(())
    }
    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_all();
        self.space.notify_one();
    }
}
pub struct Source(pub Arc<Ring>);
impl Read for Source {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut s = self.0.state.lock().unwrap();
        while s.len == 0 && !s.closed && !self.0.cancel.is_cancelled() {
            s = self
                .0
                .ready
                .wait_timeout(s, Duration::from_millis(100))
                .unwrap()
                .0;
        }
        if self.0.cancel.is_cancelled() {
            // Interrupted means "retry this read" to Read::read_exact and to
            // Symphonia. Cancellation is permanent; retrying would spin forever
            // inside the decoder and prevent the supervisor from joining it.
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let n = out.len().min(s.len);
        let first = n.min(s.data.len() - s.head);
        out[..first].copy_from_slice(&s.data[s.head..s.head + first]);
        out[first..n].copy_from_slice(&s.data[..n - first]);
        s.head = (s.head + n) % s.data.len();
        s.len -= n;
        drop(s);
        self.0.space.notify_one();
        Ok(n)
    }
}
impl Seek for Source {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
impl MediaSource for Source {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bounded_ring_wraps_and_cancels() {
        let cancel = CancellationToken::new();
        let ring = Ring::new(4, cancel.clone());
        ring.write(b"abcd").await.unwrap();
        let mut src = Source(ring.clone());
        let mut b = [0; 3];
        src.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"abc");
        ring.write(b"efg").await.unwrap();
        let mut b = [0; 4];
        src.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"defg");
        cancel.cancel();
        assert_eq!(
            src.read(&mut b).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(
            src.read_exact(&mut b).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
