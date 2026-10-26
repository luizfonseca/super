//! A request of a client, and its response: hyper's request, read and answered as Pingora's
//! server sessions are (`read_request_body`, `write_response_header`, `write_response_body`...)

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http::{header, HeaderMap, HeaderValue, StatusCode, Version};
use http_body_util::BodyExt;
use tokio::sync::{mpsc, oneshot};

use super::body::{BodyError, Part, RequestBody, ResponseBody};
use crate::{
    http::{RequestHeader, ResponseHeader},
    protocols::{l4::socket::SocketAddr, Digest},
    Error, ErrorType, Result,
};

/// What's known of a client's connection, shared by its requests
#[derive(Debug, Default)]
pub struct Connection {
    pub digest: Digest,
    pub http2: bool,
    /// Over QUIC
    pub http3: bool,
}

/// What was written of the response
#[derive(Debug, Clone, Copy)]
pub struct ResponseWritten {
    pub status: StatusCode,
}

/// Parts of a streamed body waiting to be sent: few, so a client that reads slowly slows the
/// upstream down
const PARTS: usize = 4;

/// Where the response goes
enum Responder {
    /// Nothing written yet
    Waiting(oneshot::Sender<http::Response<ResponseBody>>),
    /// Its header, sent with the first part of its body (whole bodies are sent at once)
    Pending(
        oneshot::Sender<http::Response<ResponseBody>>,
        Box<ResponseHeader>,
    ),
    /// Its body, written part by part
    Streaming(mpsc::Sender<Part>),
    Done,
}

/// A request of a client, and its response
pub struct ServerSession {
    request: RequestHeader,
    body: Option<RequestBody>,
    body_bytes_read: usize,
    responder: Responder,
    written: Option<ResponseWritten>,
    body_bytes_sent: usize,
    connection: Arc<Connection>,
    keepalive: Option<u64>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    /// The connection, once its response to an upgrade request (`101`) is sent
    upgrade: Option<hyper::upgrade::OnUpgrade>,
}

impl ServerSession {
    /// A session for `request`, whose response is sent through `responder`
    pub(crate) fn new(
        mut request: http::Request<RequestBody>,
        connection: Arc<Connection>,
        responder: oneshot::Sender<http::Response<ResponseBody>>,
    ) -> Self {
        let switching = is_upgrade(&request) || as_upgrade(&mut request);
        let upgrade = switching.then(|| hyper::upgrade::on(&mut request));
        let (parts, body) = request.into_parts();
        Self {
            request: RequestHeader::from_parts(parts),
            body: Some(body),
            body_bytes_read: 0,
            responder: Responder::Waiting(responder),
            written: None,
            body_bytes_sent: 0,
            connection,
            keepalive: Some(0),
            read_timeout: None,
            write_timeout: None,
            upgrade,
        }
    }

    /// The request is read by hyper before the session starts: always `true`
    ///
    /// # Errors
    ///
    /// Never.
    // Async as Pingora's, which reads it then
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn read_request(&mut self) -> Result<bool> {
        Ok(true)
    }

    /// Ends the response, if it wasn't
    ///
    /// # Errors
    ///
    /// When the client is gone.
    pub async fn finish(&mut self) -> Result<()> {
        self.finish_body().await
    }

    pub fn req_header(&self) -> &RequestHeader {
        &self.request
    }

    pub fn req_header_mut(&mut self) -> &mut RequestHeader {
        &mut self.request
    }

    /// The first value of the request's header `name`
    pub fn get_header(&self, name: impl header::AsHeaderName) -> Option<&HeaderValue> {
        self.request.headers.get(name)
    }

    /// The next part of the request's body, `None` at its end
    ///
    /// # Errors
    ///
    /// When the client is too slow (`set_read_timeout`) or its body can't be read.
    pub async fn read_request_body(&mut self) -> Result<Option<Bytes>> {
        self.read_body().await.map_err(Error::into_down)
    }

    async fn read_body(&mut self) -> Result<Option<Bytes>> {
        loop {
            let Some(body) = &mut self.body else {
                return Ok(None);
            };
            let frame = next_frame(body, self.read_timeout).await?;
            let frame = match frame {
                None => {
                    self.body = None;
                    return Ok(None);
                }
                Some(frame) => frame.map_err(|err| {
                    Error::because(ErrorType::ReadError, "reading the request body", err)
                })?,
            };
            // Trailers of the request aren't proxied
            if let Ok(data) = frame.into_data() {
                self.body_bytes_read += data.len();
                return Ok(Some(data));
            }
        }
    }

    /// Whether the whole request body was read
    pub fn is_body_done(&self) -> bool {
        self.body
            .as_ref()
            .is_none_or(hyper::body::Body::is_end_stream)
    }

    /// Whether the request has no body
    pub fn is_body_empty(&self) -> bool {
        self.body_bytes_read == 0 && self.is_body_done()
    }

    pub fn body_bytes_read(&self) -> usize {
        self.body_bytes_read
    }

    /// Writes the response's header, sent with the first part of its body (or when it's
    /// finished). `1xx` responses are left to hyper.
    ///
    /// # Errors
    ///
    /// When a response was already written.
    // Async as Pingora's, which writes it then (here, it's sent with the body)
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn write_response_header(&mut self, header: Box<ResponseHeader>) -> Result<()> {
        if header.status.is_informational() && header.status != StatusCode::SWITCHING_PROTOCOLS {
            return Ok(());
        }
        let Responder::Waiting(sender) = std::mem::replace(&mut self.responder, Responder::Done)
        else {
            return Err(Error::explain(
                ErrorType::InternalError,
                "response already written",
            ));
        };
        self.written = Some(ResponseWritten {
            status: header.status,
        });
        self.responder = Responder::Pending(sender, header);
        Ok(())
    }

    /// Like [`Self::write_response_header`]
    ///
    /// # Errors
    ///
    /// When a response was already written.
    pub async fn write_response_header_ref(&mut self, header: &ResponseHeader) -> Result<()> {
        self.write_response_header(Box::new(header.clone())).await
    }

    /// Sends the response's header now, before any part of its body (e.g. while the upstream's
    /// body takes long to come)
    ///
    /// # Errors
    ///
    /// When the client is gone.
    pub fn flush_header(&mut self) -> Result<()> {
        let Responder::Pending(..) = &self.responder else {
            return Ok(());
        };
        let Responder::Pending(sender, header) =
            std::mem::replace(&mut self.responder, Responder::Done)
        else {
            return Ok(());
        };
        let (parts, receiver) = mpsc::channel(PARTS);
        self.responder = Responder::Streaming(parts);
        self.send(sender, *header, ResponseBody::stream(receiver))
    }

    /// Writes a part of the response's body (the last one with `end`)
    ///
    /// # Errors
    ///
    /// When the client is gone, or too slow (`set_write_timeout`).
    pub async fn write_response_body(&mut self, data: Bytes, end: bool) -> Result<()> {
        self.body_bytes_sent += data.len();
        if let Responder::Pending(..) = &self.responder {
            if end {
                return self.finish_with(ResponseBody::full(data));
            }
            self.flush_header()?;
        }
        let Responder::Streaming(parts) = &self.responder else {
            return Err(Error::explain(
                ErrorType::InternalError,
                "no response to write to",
            ));
        };
        if !data.is_empty() {
            send_part(parts, Part::Data(data), self.write_timeout).await?;
        }
        if end {
            send_part(parts, Part::End, self.write_timeout).await?;
            self.responder = Responder::Done;
        }
        Ok(())
    }

    /// Writes the trailers of the response, ending it
    ///
    /// # Errors
    ///
    /// When the client is gone, or too slow.
    pub async fn write_response_trailers(&mut self, trailers: HeaderMap) -> Result<()> {
        self.flush_header()?;
        let Responder::Streaming(parts) = &self.responder else {
            return Ok(());
        };
        send_part(parts, Part::Trailers(trailers), self.write_timeout).await?;
        send_part(parts, Part::End, self.write_timeout).await?;
        self.responder = Responder::Done;
        Ok(())
    }

    /// Ends the response (with no more body)
    ///
    /// # Errors
    ///
    /// When the client is gone.
    pub async fn finish_body(&mut self) -> Result<()> {
        match &self.responder {
            Responder::Pending(..) => self.finish_with(ResponseBody::empty()),
            Responder::Streaming(parts) => {
                send_part(parts, Part::End, self.write_timeout).await?;
                self.responder = Responder::Done;
                Ok(())
            }
            Responder::Waiting(_) | Responder::Done => Ok(()),
        }
    }

    /// Sends the pending header with `body`, the whole body
    fn finish_with(&mut self, body: ResponseBody) -> Result<()> {
        let Responder::Pending(sender, header) =
            std::mem::replace(&mut self.responder, Responder::Done)
        else {
            return Ok(());
        };
        self.send(sender, *header, body)
    }

    fn send(
        &mut self,
        sender: oneshot::Sender<http::Response<ResponseBody>>,
        header: ResponseHeader,
        body: ResponseBody,
    ) -> Result<()> {
        let mut parts = header.into_parts();
        // HTTP/1.1 connections are closed after a response that isn't kept alive
        let switching = parts.status == StatusCode::SWITCHING_PROTOCOLS;
        if switching && self.connection.http2 {
            accept_extended_connect(&mut parts);
        }
        let multiplexed = self.connection.http2 || self.connection.http3;
        if self.keepalive.is_none() && !multiplexed && !switching {
            parts
                .headers
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
        }
        parts.version = self.request.version;
        sender
            .send(http::Response::from_parts(parts, body))
            .map_err(|_| Error::explain(ErrorType::WriteError, "the client is gone").into_down())
    }

    /// The status of the response, once written
    pub fn response_written(&self) -> Option<&ResponseWritten> {
        self.written.as_ref()
    }

    pub fn body_bytes_sent(&self) -> usize {
        self.body_bytes_sent
    }

    /// Keeps the connection open for the next request (`Some`), for up to that many seconds
    /// (hyper's own idle timeout applies), or closes it after this response (`None`)
    pub fn set_keepalive(&mut self, seconds: Option<u64>) {
        self.keepalive = seconds;
    }

    pub fn get_keepalive(&self) -> Option<u64> {
        self.keepalive
    }

    /// How long each read of the request body can wait
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.read_timeout = timeout;
    }

    /// How long each write of the response body can wait for the client
    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        self.write_timeout = timeout;
    }

    pub fn digest(&self) -> Option<&Digest> {
        Some(&self.connection.digest)
    }

    pub fn client_addr(&self) -> Option<&SocketAddr> {
        self.connection.digest.socket_digest.as_ref()?.peer_addr()
    }

    pub fn server_addr(&self) -> Option<&SocketAddr> {
        self.connection.digest.socket_digest.as_ref()?.local_addr()
    }

    pub fn is_http2(&self) -> bool {
        self.connection.http2
    }

    pub fn is_http3(&self) -> bool {
        self.connection.http3
    }

    /// Whether the client asks to switch protocols (e.g. a websocket, over HTTP/1.1 or HTTP/2)
    pub fn is_upgrade_req(&self) -> bool {
        self.upgrade.is_some()
    }

    /// The connection of an upgrade request, available once its `101` response is sent
    pub fn take_upgrade(&mut self) -> Option<hyper::upgrade::OnUpgrade> {
        self.upgrade.take()
    }

    /// The request's body, to read it elsewhere (e.g. to stream it to the upstream)
    pub fn take_body(&mut self) -> Option<RequestBody> {
        self.body.take()
    }

    pub fn version(&self) -> Version {
        self.request.version
    }
}

/// The next frame of a request body: what's there already, else waiting at most `timeout`
async fn next_frame(
    body: &mut RequestBody,
    timeout: Option<Duration>,
) -> Result<Option<std::result::Result<http_body::Frame<Bytes>, BodyError>>> {
    // There already: no timer
    if let Some(frame) = futures_util::FutureExt::now_or_never(body.frame()) {
        return Ok(frame);
    }
    let Some(timeout) = timeout else {
        return Ok(body.frame().await);
    };
    tokio::time::timeout(timeout, body.frame())
        .await
        .map_err(|_| Error::explain(ErrorType::ReadTimedout, "reading the request body"))
}

/// Sends a part of a streamed body, waiting for room at most `timeout`
async fn send_part(
    parts: &mpsc::Sender<Part>,
    part: Part,
    timeout: Option<Duration>,
) -> Result<()> {
    let gone = || Error::explain(ErrorType::WriteError, "the client is gone").into_down();
    // Room already (the client keeps up): no timer
    let part = match parts.try_send(part) {
        Ok(()) => return Ok(()),
        Err(mpsc::error::TrySendError::Closed(_)) => return Err(gone()),
        Err(mpsc::error::TrySendError::Full(part)) => part,
    };
    let Some(timeout) = timeout else {
        return parts.send(part).await.map_err(|_| gone());
    };
    match tokio::time::timeout(timeout, parts.send(part)).await {
        Ok(sent) => sent.map_err(|_| gone()),
        Err(_) => {
            Err(Error::explain(ErrorType::WriteTimedout, "writing the response body").into_down())
        }
    }
}

/// Whether `request` asks to switch protocols (HTTP/1.1 only)
fn is_upgrade<B>(request: &http::Request<B>) -> bool {
    request.version() == Version::HTTP_11
        && request.headers().contains_key(header::UPGRADE)
        && request
            .headers()
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
}

/// An HTTP/2 client's request for another protocol (an extended `CONNECT`, RFC 8441), turned
/// into the HTTP/1.1 upgrade request it stands for: the phases, and the upstream, see the
/// request as an HTTP/1.1 client would send it. Returns whether it was one.
fn as_upgrade<B>(request: &mut http::Request<B>) -> bool {
    if request.method() != http::Method::CONNECT {
        return false;
    }
    let Some(protocol) = request.extensions_mut().remove::<hyper::ext::Protocol>() else {
        return false;
    };
    let Ok(upgrade) = HeaderValue::from_bytes(protocol.as_ref()) else {
        return false;
    };
    let websocket = protocol.as_str().eq_ignore_ascii_case("websocket");
    *request.method_mut() = http::Method::GET;
    let headers = request.headers_mut();
    headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    headers.insert(header::UPGRADE, upgrade);
    // HTTP/2 websockets have no key (RFC 8441, 5): the upstream wants one
    if websocket && !headers.contains_key(header::SEC_WEBSOCKET_KEY) {
        if let Some(key) = websocket_key() {
            headers.insert(header::SEC_WEBSOCKET_KEY, key);
        }
    }
    true
}

/// A websocket key: 16 random bytes, in base64 (RFC 6455, 4.1)
fn websocket_key() -> Option<HeaderValue> {
    let mut nonce = [0u8; 16];
    openssl::rand::rand_bytes(&mut nonce).ok()?;
    HeaderValue::from_str(&openssl::base64::encode_block(&nonce)).ok()
}

/// The upstream switched protocols: an HTTP/2 client is told with a `200`, and without the
/// headers of an HTTP/1.1 upgrade (RFC 8441, 5)
fn accept_extended_connect(parts: &mut http::response::Parts) {
    parts.status = StatusCode::OK;
    for name in [
        header::CONNECTION,
        header::UPGRADE,
        header::SEC_WEBSOCKET_ACCEPT,
    ] {
        parts.headers.remove(name);
    }
}
