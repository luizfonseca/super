//! The bodies of a client's request (read by hyper, or from an HTTP/3 stream), and of its
//! response, as hyper sends it: none, whole, or parts written as the request's phases produce them

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use hyper::body::Incoming;
use tokio::sync::mpsc;

/// The receiving side of an HTTP/3 request's stream
pub(crate) type H3Stream = h3::server::RequestStream<h3_quinn::RecvStream, Bytes>;

/// Why a request body couldn't be read
pub type BodyError = Box<dyn std::error::Error + Send + Sync>;

/// The body of a client's request
pub struct RequestBody {
    kind: RequestKind,
    /// What was read ahead (to know whether there's more)
    pending: Option<Result<Frame<Bytes>, BodyError>>,
}

enum RequestKind {
    /// HTTP/1.1 and HTTP/2
    Hyper(Incoming),
    /// HTTP/3: its data, then its trailers
    H3 {
        stream: H3Stream,
        data_done: bool,
    },
    Done,
}

impl RequestBody {
    pub(crate) fn hyper(body: Incoming) -> Self {
        Self {
            kind: RequestKind::Hyper(body),
            pending: None,
        }
    }

    /// The body of an HTTP/3 stream: what's there already is read, so that a stream that ended
    /// with its headers (e.g. a `GET`) is known to have no body, as hyper knows it
    pub(crate) fn h3(stream: H3Stream) -> Self {
        let mut body = Self {
            kind: RequestKind::H3 {
                stream,
                data_done: false,
            },
            pending: None,
        };
        let polled = futures_util::FutureExt::now_or_never(std::future::poll_fn(|cx| {
            Pin::new(&mut body).poll_frame(cx)
        }));
        if let Some(Some(frame)) = polled {
            body.pending = Some(frame);
        }
        body
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        if let Some(frame) = self.pending.take() {
            return Poll::Ready(Some(frame));
        }
        let (stream, data_done) = match &mut self.kind {
            RequestKind::Done => return Poll::Ready(None),
            RequestKind::Hyper(body) => {
                return Pin::new(body)
                    .poll_frame(cx)
                    .map(|frame| frame.map(|frame| frame.map_err(Into::into)));
            }
            RequestKind::H3 { stream, data_done } => (stream, data_done),
        };
        if !*data_done {
            match stream.poll_recv_data(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(err)) => return Poll::Ready(Some(Err(err.into()))),
                // quinn's parts are `Bytes` already: taken as they are
                Poll::Ready(Ok(Some(mut data))) => {
                    let data = data.copy_to_bytes(data.remaining());
                    return Poll::Ready(Some(Ok(Frame::data(data))));
                }
                Poll::Ready(Ok(None)) => *data_done = true,
            }
        }
        let frame = match stream.poll_recv_trailers(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => Some(Err(err.into())),
            Poll::Ready(Ok(trailers)) => trailers.map(|trailers| Ok(Frame::trailers(trailers))),
        };
        self.kind = RequestKind::Done;
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        if self.pending.is_some() {
            return false;
        }
        match &self.kind {
            RequestKind::Hyper(body) => body.is_end_stream(),
            RequestKind::H3 { .. } => false,
            RequestKind::Done => true,
        }
    }
}

/// What the session writes to a streamed body
pub(crate) enum Part {
    Data(Bytes),
    Trailers(HeaderMap),
    /// The body ended: without it, a body whose writer is gone was cut short
    End,
}

/// What's left of a request's handling once its response is sent: it writes the rest of the
/// body
pub(crate) type Driver = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The body of a response
pub struct ResponseBody {
    kind: Kind,
}

enum Kind {
    Empty,
    Full(Option<Bytes>),
    /// Written as the request's handling goes on, which this body runs (in the connection's task)
    Stream {
        parts: mpsc::Receiver<Part>,
        driver: Option<Driver>,
    },
    Done,
}

impl ResponseBody {
    pub fn empty() -> Self {
        Self { kind: Kind::Empty }
    }

    pub fn full(data: Bytes) -> Self {
        if data.is_empty() {
            return Self::empty();
        }
        Self {
            kind: Kind::Full(Some(data)),
        }
    }

    pub(crate) fn stream(parts: mpsc::Receiver<Part>) -> Self {
        Self {
            kind: Kind::Stream {
                parts,
                driver: None,
            },
        }
    }

    /// The rest of the request's handling, run as the body is read
    pub(crate) fn drive(&mut self, rest: Driver) {
        if let Kind::Stream { driver, .. } = &mut self.kind {
            *driver = Some(rest);
            return;
        }
        // Nothing left to write: it only finishes (e.g. its logging)
        tokio::spawn(rest);
    }
}

/// A body cut short, its writer gone before its end
#[derive(Debug)]
pub struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the response body was cut short")
    }
}

impl std::error::Error for Aborted {}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = Aborted;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Aborted>>> {
        let this = &mut *self;
        let (parts, driver) = match &mut this.kind {
            Kind::Empty | Kind::Done => return Poll::Ready(None),
            Kind::Full(data) => return Poll::Ready(data.take().map(|data| Ok(Frame::data(data)))),
            Kind::Stream { parts, driver } => (parts, driver),
        };
        // The handling writes the parts: it goes on until it waits, or ends
        if let Some(running) = driver {
            if running.as_mut().poll(cx).is_ready() {
                *driver = None;
            }
        }
        let frame = match parts.poll_recv(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(Part::Data(data))) => Some(Ok(Frame::data(data))),
            Poll::Ready(Some(Part::Trailers(trailers))) => Some(Ok(Frame::trailers(trailers))),
            Poll::Ready(Some(Part::End)) => None,
            Poll::Ready(None) => Some(Err(Aborted)),
        };
        if !matches!(frame, Some(Ok(_))) {
            this.finish();
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        match &self.kind {
            Kind::Empty | Kind::Done => true,
            Kind::Full(data) => data.is_none(),
            Kind::Stream { .. } => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.kind {
            Kind::Empty | Kind::Done => SizeHint::with_exact(0),
            Kind::Full(data) => {
                SizeHint::with_exact(data.as_ref().map_or(0, |data| data.len() as u64))
            }
            Kind::Stream { .. } => SizeHint::default(),
        }
    }
}

impl ResponseBody {
    /// The body is over: what's left of the handling (e.g. its logging) finishes on its own
    fn finish(&mut self) {
        if let Kind::Stream {
            driver: Some(rest), ..
        } = std::mem::replace(&mut self.kind, Kind::Done)
        {
            tokio::spawn(rest);
        }
    }
}

impl Drop for ResponseBody {
    /// A client gone before the end: the handling still finishes (its writes fail), and logs
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use tokio::sync::mpsc;

    use super::{Part, ResponseBody};

    #[tokio::test]
    async fn test_bodies_whole_or_streamed() {
        let body = ResponseBody::full(Bytes::from_static(b"pong"));
        assert_eq!(body.collect().await.unwrap().to_bytes(), "pong");

        let (sender, parts) = mpsc::channel(1);
        let mut body = ResponseBody::stream(parts);
        body.drive(Box::pin(async move {
            for part in ["a", "b"] {
                sender.send(Part::Data(Bytes::from(part))).await.ok();
            }
            sender.send(Part::End).await.ok();
        }));
        assert_eq!(body.collect().await.unwrap().to_bytes(), "ab");
    }

    #[tokio::test]
    async fn test_bodies_cut_short() {
        let (sender, parts) = mpsc::channel(1);
        let mut body = ResponseBody::stream(parts);
        body.drive(Box::pin(async move {
            sender.send(Part::Data(Bytes::from("a"))).await.ok();
        }));
        assert!(body.collect().await.is_err());
    }
}
