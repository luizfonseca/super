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
use crate::protocols::{
    proxy_protocol::{self, Parsed},
    SocketDigest, IO,
};

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

    /// The IP address of the peer (`None` over a Unix socket)
    pub fn peer_ip(&self) -> Option<std::net::IpAddr> {
        self.digest
            .peer_addr()
            .and_then(SocketAddr::as_inet)
            .map(std::net::SocketAddr::ip)
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
            if !self.read_ahead().await? {
                return Ok(false);
            }
        }
        let ahead = self
            .ahead
            .get(self.ahead_at..self.ahead_at + buf.len())
            .unwrap_or_default();
        buf.copy_from_slice(ahead);
        Ok(true)
    }

    /// Reads the PROXY protocol header (v1 or v2) the connection starts with, if it does: its
    /// client becomes the connection's peer. The connection's own bytes, read ahead to tell,
    /// are read again by whoever reads it next.
    ///
    /// # Errors
    ///
    /// When the header doesn't follow the protocol, or the connection fails.
    pub async fn read_proxy_header(&mut self) -> io::Result<()> {
        loop {
            let unread = self.ahead.get(self.ahead_at..).unwrap_or_default();
            match proxy_protocol::parse(unread) {
                Parsed::NotProxy => return Ok(()),
                Parsed::Invalid => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid PROXY protocol header",
                    ))
                }
                Parsed::Header { len, source } => {
                    self.take_proxy_header(len, source);
                    return Ok(());
                }
                Parsed::Incomplete => {}
            }
            // Closed before the header ended: whoever reads it next finds what came
            if !self.read_ahead().await? {
                return Ok(());
            }
        }
    }

    /// The PROXY protocol header of `len` bytes read ahead is done with: its client, if it has
    /// one, becomes the peer
    fn take_proxy_header(&mut self, len: usize, source: Option<std::net::SocketAddr>) {
        self.consume_ahead(len);
        if let Some(source) = source {
            // Not shared yet: changed in place
            Arc::make_mut(&mut self.digest).peer_addr = Some(SocketAddr::Inet(source));
        }
    }

    /// Reads what the connection has sent (up to 2 KiB) ahead. `false` when it's closed.
    async fn read_ahead(&mut self) -> io::Result<bool> {
        let mut chunk = [0u8; 2048];
        let read = match &mut self.socket {
            Socket::Tcp(stream) => stream.read(&mut chunk).await?,
            Socket::Unix(stream) => stream.read(&mut chunk).await?,
        };
        self.ahead
            .extend_from_slice(chunk.get(..read).unwrap_or_default());
        Ok(read > 0)
    }

    /// The next `len` bytes read ahead are done with: not read again
    fn consume_ahead(&mut self, len: usize) {
        self.ahead_at += len;
        if self.ahead_at >= self.ahead.len() {
            self.ahead = Vec::new();
            self.ahead_at = 0;
        }
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

    /// A connection on which a client sends each of `writes`, a moment apart
    async fn connection(writes: &[&'static [u8]]) -> Stream {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let writes = writes.to_vec();
        tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            for write in writes {
                client.write_all(write).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });
        let (socket, _) = listener.accept().await.unwrap();
        Stream::from(socket)
    }

    fn peer(stream: &Stream) -> String {
        let digest = crate::protocols::IO::get_socket_digest(stream).unwrap();
        digest.peer_addr().unwrap().to_string()
    }

    async fn rest(mut stream: Stream) -> String {
        let mut rest = String::new();
        stream.read_to_string(&mut rest).await.unwrap();
        rest
    }

    #[tokio::test]
    async fn test_proxy_headers_give_the_client() {
        let mut stream = connection(&[b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 443\r\nGET /"]).await;
        stream.read_proxy_header().await.unwrap();
        assert_eq!(peer(&stream), "203.0.113.7:51234");
        assert_eq!(rest(stream).await, "GET /");

        // In pieces
        let mut stream = connection(&[b"PROXY TCP6 2001:db8::7 ", b"2001:db8::1 1 443\r\nhi"]).await;
        stream.read_proxy_header().await.unwrap();
        assert_eq!(peer(&stream), "[2001:db8::7]:1");
        assert_eq!(rest(stream).await, "hi");
    }

    #[tokio::test]
    async fn test_connections_without_a_proxy_header_are_left_as_they_are() {
        let mut stream = connection(&[b"GET / HTTP/1.1\r\n"]).await;
        let before = peer(&stream);
        stream.read_proxy_header().await.unwrap();
        assert_eq!(peer(&stream), before);
        assert_eq!(rest(stream).await, "GET / HTTP/1.1\r\n");

        // A header without a client (the load balancer's own connection): consumed
        let mut stream = connection(&[b"PROXY UNKNOWN\r\nping"]).await;
        stream.read_proxy_header().await.unwrap();
        assert!(peer(&stream).starts_with("127.0.0.1:"), "{}", peer(&stream));
        assert_eq!(rest(stream).await, "ping");
    }

    #[tokio::test]
    async fn test_invalid_proxy_headers_are_errors() {
        let mut stream = connection(&[b"PROXY TCP4 nope\r\n"]).await;
        let err = stream.read_proxy_header().await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
