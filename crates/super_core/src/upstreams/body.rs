//! The body of a request to an upstream: none, whole (e.g. sent again from the retry buffer), or
//! the client's, part by part as the request's phases pass it on

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use tokio::sync::mpsc;

use crate::protocols::http::body::{Aborted, Part};

pub struct UpstreamBody {
    kind: Kind,
}

enum Kind {
    Empty,
    Full(Option<Bytes>),
    Stream(mpsc::Receiver<Part>),
}

impl UpstreamBody {
    pub fn empty() -> Self {
        Self { kind: Kind::Empty }
    }

    pub fn full(data: Bytes) -> Self {
        Self {
            kind: Kind::Full((!data.is_empty()).then_some(data)),
        }
    }

    /// A body whose parts are sent to the returned sender (ended with [`Part::End`])
    pub(crate) fn stream(room: usize) -> (mpsc::Sender<Part>, Self) {
        let (sender, parts) = mpsc::channel(room);
        (
            sender,
            Self {
                kind: Kind::Stream(parts),
            },
        )
    }
}

impl Body for UpstreamBody {
    type Data = Bytes;
    type Error = Aborted;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Aborted>>> {
        let parts = match &mut self.kind {
            Kind::Empty => return Poll::Ready(None),
            Kind::Full(data) => return Poll::Ready(data.take().map(|data| Ok(Frame::data(data)))),
            Kind::Stream(parts) => parts,
        };
        let frame = match parts.poll_recv(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(Part::Data(data))) => Some(Ok(Frame::data(data))),
            Poll::Ready(Some(Part::Trailers(trailers))) => Some(Ok(Frame::trailers(trailers))),
            Poll::Ready(Some(Part::End)) => None,
            // The client's body was cut short: so is the upstream's
            Poll::Ready(None) => Some(Err(Aborted)),
        };
        if !matches!(frame, Some(Ok(_))) {
            self.kind = Kind::Empty;
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        match &self.kind {
            Kind::Empty => true,
            Kind::Full(data) => data.is_none(),
            Kind::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.kind {
            Kind::Empty => SizeHint::with_exact(0),
            Kind::Full(data) => {
                SizeHint::with_exact(data.as_ref().map_or(0, |data| data.len() as u64))
            }
            Kind::Stream(_) => SizeHint::default(),
        }
    }
}
