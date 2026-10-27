//! Lingering close (as nginx's): an HTTP/1.1 connection closed while its client still sends (an
//! upload answered early, with a `413` or a `401`) is reset by the client's kernel, which can
//! drop the answer before the client reads it. Once its side is closed, Super keeps reading, and
//! discarding, what the client sends for a while, so the client reads the answer first.

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{ready, Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::{sleep, Sleep},
};

/// How long a closed connection is read at most, and without anything coming
const LINGER: Duration = Duration::from_secs(10);
const LINGER_IDLE: Duration = Duration::from_secs(2);

/// A connection that lingers once it's shut down, unless `upgraded` (its bytes then go to the
/// upstream, through a tunnel)
pub(crate) struct Lingering<S> {
    inner: S,
    upgraded: Arc<AtomicBool>,
    /// Shut down, and until when it's read
    closing: Option<Closing>,
    /// Shut down, and read
    closed: bool,
}

struct Closing {
    until: Pin<Box<Sleep>>,
    idle: Pin<Box<Sleep>>,
}

impl<S> Lingering<S> {
    pub(crate) fn new(inner: S, upgraded: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            upgraded,
            closing: None,
            closed: false,
        }
    }
}

impl<S: AsyncRead + Unpin> Lingering<S> {
    /// Reads and discards until the client closes, fails or stops sending: ready then
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let mut buffer = [0u8; 8192];
        loop {
            let Some(closing) = self.closing.as_mut() else {
                return Poll::Ready(());
            };
            if closing.until.as_mut().poll(cx).is_ready()
                || closing.idle.as_mut().poll(cx).is_ready()
            {
                return Poll::Ready(());
            }
            let mut read = ReadBuf::new(&mut buffer);
            match Pin::new(&mut self.inner).poll_read(cx, &mut read) {
                Poll::Ready(Ok(())) if !read.filled().is_empty() => closing.wait_again(),
                Poll::Ready(_) => return Poll::Ready(()),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Closing {
    fn new() -> Self {
        Self {
            until: Box::pin(sleep(LINGER)),
            idle: Box::pin(sleep(LINGER_IDLE)),
        }
    }

    /// Something came: it waits a while longer
    fn wait_again(&mut self) {
        let deadline = tokio::time::Instant::now() + LINGER_IDLE;
        self.idle.as_mut().reset(deadline);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Lingering<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Lingering<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(Ok(()));
        }
        if this.closing.is_none() {
            ready!(Pin::new(&mut this.inner).poll_shutdown(cx))?;
            if this.upgraded.load(Ordering::Relaxed) {
                this.closed = true;
                return Poll::Ready(Ok(()));
            }
            this.closing = Some(Closing::new());
        }
        ready!(this.poll_drain(cx));
        this.closing = None;
        this.closed = true;
        Poll::Ready(Ok(()))
    }
}
