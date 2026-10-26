//! A TCP or Unix socket connection, whose first bytes can be read ahead (a TLS `ClientHello`)
//! and read again by whoever handles it

use std::{
    any::Any,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    net::{TcpStream, UnixStream},
};

use super::socket::SocketAddr;
use crate::protocols::{SocketDigest, IO};

#[derive(Debug)]
enum Socket {
    Tcp(TcpStream),
    Unix(UnixStream),
}

/// A connection, with the bytes read ahead of whoever handles it
#[derive(Debug)]
pub struct Stream {
    socket: Socket,
    /// Read ahead, and not read again yet (from `ahead_at`)
    ahead: Vec<u8>,
    ahead_at: usize,
    digest: Arc<SocketDigest>,
}

impl Stream {
    pub fn tcp(stream: TcpStream) -> Self {
        let digest = SocketDigest {
            peer_addr: stream.peer_addr().ok().map(SocketAddr::Inet),
            local_addr: stream.local_addr().ok().map(SocketAddr::Inet),
        };
        Self::new(Socket::Tcp(stream), digest)
    }

    pub fn unix(stream: UnixStream) -> Self {
        let path = |addr: tokio::net::unix::SocketAddr| {
            addr.as_pathname()
                .map(|path| SocketAddr::Unix(path.to_path_buf()))
        };
        let digest = SocketDigest {
            peer_addr: stream.peer_addr().ok().and_then(path),
            local_addr: stream.local_addr().ok().and_then(path),
        };
        Self::new(Socket::Unix(stream), digest)
    }

    fn new(socket: Socket, digest: SocketDigest) -> Self {
        Self {
            socket,
            ahead: Vec::new(),
            ahead_at: 0,
            digest: Arc::new(digest),
        }
    }

    pub fn as_tcp(&self) -> Option<&TcpStream> {
        match &self.socket {
            Socket::Tcp(stream) => Some(stream),
            Socket::Unix(_) => None,
        }
    }

    /// The first `buf.len()` bytes not read yet, read again by whoever reads the stream next.
    /// `false` when the connection ends before.
    ///
    /// # Errors
    ///
    /// When the connection fails.
    pub async fn try_peek(&mut self, buf: &mut [u8]) -> io::Result<bool> {
        while self.ahead.len() - self.ahead_at < buf.len() {
            let mut chunk = [0u8; 2048];
            let read = match &mut self.socket {
                Socket::Tcp(stream) => stream.read(&mut chunk).await?,
                Socket::Unix(stream) => stream.read(&mut chunk).await?,
            };
            if read == 0 {
                return Ok(false);
            }
            self.ahead
                .extend_from_slice(chunk.get(..read).unwrap_or_default());
        }
        let ahead = self
            .ahead
            .get(self.ahead_at..self.ahead_at + buf.len())
            .unwrap_or_default();
        buf.copy_from_slice(ahead);
        Ok(true)
    }

    fn set_nodelay(&self) {
        if let Socket::Tcp(stream) = &self.socket {
            let _ = stream.set_nodelay(true);
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.ahead_at < this.ahead.len() {
            let ahead = this.ahead.get(this.ahead_at..).unwrap_or_default();
            let read = ahead.len().min(buf.remaining());
            buf.put_slice(ahead.get(..read).unwrap_or_default());
            this.ahead_at += read;
            if this.ahead_at == this.ahead.len() {
                // Read again: no longer kept
                this.ahead = Vec::new();
                this.ahead_at = 0;
            }
            return Poll::Ready(Ok(()));
        }
        match &mut this.socket {
            Socket::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Socket::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.socket {
            Socket::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Socket::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match &mut self.socket {
            Socket::Tcp(stream) => Pin::new(stream).poll_write_vectored(cx, bufs),
            Socket::Unix(stream) => Pin::new(stream).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.socket {
            Socket::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Socket::Unix(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.socket {
            Socket::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Socket::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

impl IO for Stream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        Some(self.digest.clone())
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl From<TcpStream> for Stream {
    fn from(stream: TcpStream) -> Self {
        let stream = Self::tcp(stream);
        stream.set_nodelay();
        stream
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::Stream;

    #[tokio::test]
    async fn test_bytes_read_ahead_are_read_again() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            client.write_all(b"hello world").await.unwrap();
        });
        let (socket, _) = listener.accept().await.unwrap();
        let mut stream = Stream::from(socket);
        client.await.unwrap();

        let mut peeked = [0u8; 5];
        assert!(stream.try_peek(&mut peeked).await.unwrap());
        assert_eq!(&peeked, b"hello");

        let mut all = String::new();
        stream.read_to_string(&mut all).await.unwrap();
        assert_eq!(all, "hello world");

        let mut more = [0u8; 1];
        assert!(!stream.try_peek(&mut more).await.unwrap());
    }
}
