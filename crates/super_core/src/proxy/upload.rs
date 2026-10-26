//! The request body, sent to the upstream as it's read from the client, through
//! `request_body_filter`, while the upstream's response is awaited and read (both ways at once).
//! What's sent is kept (up to 64 KiB) to send it again when the request is retried.

use std::{future::Future, pin::pin, time::Duration};

use bytes::{Bytes, BytesMut};
use tokio::sync::mpsc;

use super::{HttpProxy, ProxyHttp, Session};
use crate::{protocols::http::body::Part, upstreams::body::UpstreamBody, Error, ErrorType, Result};

/// Parts of the request body waiting to be sent: a slow upstream slows the client down
const ROOM: usize = 4;

/// The request body being sent
pub(super) struct Upload {
    sender: Option<mpsc::Sender<Part>>,
    /// The next part to send
    pending: Option<Part>,
    /// The end, to send after `pending`
    then_end: bool,
    /// Parts already sent to a previous upstream, sent first
    replay: std::vec::IntoIter<Bytes>,
    /// Whether the client's body has parts left
    reading: bool,
    write_timeout: Option<Duration>,
}

impl Upload {
    fn done(write_timeout: Option<Duration>) -> Self {
        Self {
            sender: None,
            pending: None,
            then_end: false,
            replay: Vec::new().into_iter(),
            reading: false,
            write_timeout,
        }
    }

    /// Whether the whole body was sent
    pub(super) fn is_done(&self) -> bool {
        self.sender.is_none()
    }

    /// The body of the request to the upstream, and what sends it
    pub(super) async fn start<SV: ProxyHttp>(
        proxy: &HttpProxy<SV>,
        session: &mut Session,
        ctx: &mut SV::CTX,
        write_timeout: Option<Duration>,
    ) -> Result<(Self, UpstreamBody)> {
        // Tried again, with the whole body: sent as it was
        if session.request_body_sent {
            let body = whole(&session.retry_buffer.parts);
            return Ok((Self::done(write_timeout), body));
        }
        let started = !session.retry_buffer.parts.is_empty();
        // No body: the filter still sees its end
        if !started && session.downstream_session.is_body_done() {
            let mut body = None;
            proxy
                .inner
                .request_body_filter(session, &mut body, true, ctx)
                .await?;
            session.request_body_sent = true;
            if let Some(data) = &body {
                session.retry_buffer.keep(data);
            }
            let body = body.map_or_else(UpstreamBody::empty, UpstreamBody::full);
            return Ok((Self::done(write_timeout), body));
        }
        let (sender, body) = UpstreamBody::stream(ROOM);
        let upload = Self {
            sender: Some(sender),
            pending: None,
            then_end: false,
            replay: session.retry_buffer.parts.clone().into_iter(),
            reading: true,
            write_timeout,
        };
        Ok((upload, body))
    }

    /// A part read from the client (`None`: its end), through the filter
    async fn input<SV: ProxyHttp>(
        &mut self,
        proxy: &HttpProxy<SV>,
        session: &mut Session,
        ctx: &mut SV::CTX,
        read: Option<Bytes>,
    ) -> Result<()> {
        let end = read.is_none() || session.downstream_session.is_body_done();
        let mut body = read;
        proxy
            .inner
            .request_body_filter(session, &mut body, end, ctx)
            .await?;
        if let Some(data) = body.filter(|data| !data.is_empty()) {
            session.retry_buffer.keep(&data);
            self.pending = Some(Part::Data(data));
            self.then_end = end;
        } else if end {
            self.pending = Some(Part::End);
        }
        if end {
            self.reading = false;
        }
        Ok(())
    }

    /// A part was sent: the end next, if it's due
    fn sent(&mut self, session: &mut Session, end: bool) {
        if end {
            self.sender = None;
            session.request_body_sent = true;
        } else if self.then_end {
            self.then_end = false;
            self.pending = Some(Part::End);
        }
    }
}

/// The body of `parts`, in one part
fn whole(parts: &[Bytes]) -> UpstreamBody {
    match parts {
        [] => UpstreamBody::empty(),
        [part] => UpstreamBody::full(part.clone()),
        parts => {
            let mut body = BytesMut::with_capacity(parts.iter().map(Bytes::len).sum());
            for part in parts {
                body.extend_from_slice(part);
            }
            UpstreamBody::full(body.freeze())
        }
    }
}

/// Runs `main` (e.g. waiting for the upstream's response) while sending the request body
pub(super) async fn alongside<SV: ProxyHttp, F: Future>(
    main: F,
    upload: &mut Upload,
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
) -> Result<F::Output> {
    let mut main = pin!(main);
    loop {
        let Some(sender) = upload.sender.clone() else {
            return Ok(main.await);
        };
        if upload.pending.is_none() {
            upload.pending = upload.replay.next().map(Part::Data);
        }
        let reading = upload.reading && upload.pending.is_none();
        let sending = upload.pending.is_some();
        let write_timeout = upload.write_timeout;
        tokio::select! {
            biased;
            output = &mut main => return Ok(output),
            read = session.downstream_session.read_request_body(), if reading => {
                upload.input(proxy, session, ctx, read?).await?;
            }
            room = reserve(&sender, write_timeout), if sending => {
                let permit = room?;
                let end = matches!(upload.pending, Some(Part::End));
                if let Some(part) = upload.pending.take() {
                    permit.send(part);
                }
                upload.sent(session, end);
            }
        }
    }
}

/// Room for a part, waiting for the upstream at most `timeout`
async fn reserve(
    sender: &mpsc::Sender<Part>,
    timeout: Option<Duration>,
) -> Result<mpsc::Permit<'_, Part>> {
    let gone = || Error::explain(ErrorType::WriteError, "the upstream stopped reading").into_up();
    let Some(timeout) = timeout else {
        return sender.reserve().await.map_err(|_| gone());
    };
    match tokio::time::timeout(timeout, sender.reserve()).await {
        Ok(permit) => permit.map_err(|_| gone()),
        Err(_) => {
            Err(Error::explain(ErrorType::WriteTimedout, "sending the request body").into_up())
        }
    }
}
