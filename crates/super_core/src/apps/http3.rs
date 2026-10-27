//! HTTP/3 connections (over QUIC): each request stream is a [`ServerSession`], answered as those
//! of HTTP/1.1 and HTTP/2 connections are

use std::sync::Arc;

use bytes::Bytes;
use http_body::Body;
use http_body_util::BodyExt;

use super::{
    http::{handle, MAX_H2_HEADER_LIST},
    HttpServerApp,
};
use crate::{
    protocols::{
        http::{body::RequestBody, Connection, ResponseBody},
        Digest,
    },
    server::ShutdownWatch,
};

/// The sending side of a request's stream
type Responder = h3::server::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;

/// Serves the requests of `connection` until it closes. Once shutting down, the client is told
/// to go away (`GOAWAY`): the requests in progress finish.
pub async fn serve_quic<A: HttpServerApp>(
    app: &Arc<A>,
    connection: quinn::Connection,
    digest: Digest,
    shutdown: &ShutdownWatch,
) {
    let connection = h3_quinn::Connection::new(connection);
    let built = h3::server::builder()
        .max_field_section_size(u64::from(MAX_H2_HEADER_LIST))
        .build::<_, Bytes>(connection)
        .await;
    let mut h3 = match built {
        Ok(h3) => h3,
        Err(err) => {
            tracing::debug!("HTTP/3 connection failed: {err}");
            return;
        }
    };
    let connection = Arc::new(Connection {
        digest,
        http2: false,
        http3: true,
    });
    let mut shutdown = shutdown.clone();
    let mut stopping = *shutdown.borrow();
    if stopping {
        let _ = h3.shutdown(0).await;
    }
    // Without a server to say so, it never shuts down
    let mut watching = true;
    loop {
        tokio::select! {
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => return,
                    Err(err) => {
                        if !err.is_h3_no_error() {
                            tracing::debug!("HTTP/3 connection closed: {err}");
                        }
                        return;
                    }
                };
                let (app, connection, shutdown) = (app.clone(), connection.clone(), shutdown.clone());
                tokio::spawn(async move {
                    let request = match resolver.resolve_request().await {
                        Ok(request) => request,
                        Err(err) => {
                            tracing::debug!("HTTP/3 request failed: {err}");
                            return;
                        }
                    };
                    answer(app, connection, shutdown, request, stopping).await;
                });
            }
            changed = shutdown.changed(), if watching => {
                watching = changed.is_ok() && !*shutdown.borrow();
                if changed.is_ok() && !watching {
                    stopping = true;
                    let _ = h3.shutdown(0).await;
                }
            }
        }
    }
}

/// Answers a request of the connection
async fn answer<A: HttpServerApp>(
    app: Arc<A>,
    connection: Arc<Connection>,
    shutdown: ShutdownWatch,
    (request, mut stream): (
        http::Request<()>,
        h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    ),
    stopping: bool,
) {
    if is_malformed(&request) {
        stream.stop_sending(h3::error::Code::H3_MESSAGE_ERROR);
        stream.stop_stream(h3::error::Code::H3_MESSAGE_ERROR);
        return;
    }
    let (mut responder, body) = stream.split();
    let request = request.map(|()| RequestBody::h3(body));
    match handle(app, connection, shutdown, request, ((), stopping)).await {
        Ok(response) => respond(&mut responder, response).await,
        Err(_) => responder.stop_stream(h3::error::Code::H3_INTERNAL_ERROR),
    }
}

/// Whether a request has a header of HTTP/1.1's connections, which HTTP/3 requests can't have
/// (RFC 9114 §4.2): `connection`, `keep-alive`, `proxy-connection`, `transfer-encoding`, `upgrade`,
/// or `te` other than `trailers`
fn is_malformed(request: &http::Request<()>) -> bool {
    use http::header::{CONNECTION, TE, TRANSFER_ENCODING, UPGRADE};
    let headers = request.headers();
    let connection_specific = [CONNECTION, TRANSFER_ENCODING, UPGRADE]
        .iter()
        .any(|name| headers.contains_key(name))
        || headers.contains_key("keep-alive")
        || headers.contains_key("proxy-connection");
    connection_specific || headers.get_all(TE).iter().any(|te| te != "trailers")
}

/// Sends `response` on the request's stream: its header, then its body, as hyper would
async fn respond(responder: &mut Responder, response: hyper::Response<ResponseBody>) {
    let (mut parts, mut body) = response.into_parts();
    // A whole body's length (an empty one may be a `HEAD`'s, whose length is the upstream's)
    if let Some(length) = body.size_hint().exact().filter(|length| *length > 0) {
        parts
            .headers
            .entry(http::header::CONTENT_LENGTH)
            .or_insert_with(|| http::HeaderValue::from(length));
    }
    let header = http::Response::from_parts(parts, ());
    if let Err(err) = responder.send_response(header).await {
        tracing::debug!("HTTP/3 response failed: {err}");
        return;
    }
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            // Cut short: the client mustn't take it as whole
            responder.stop_stream(h3::error::Code::H3_INTERNAL_ERROR);
            return;
        };
        if let Err(err) = send(responder, frame).await {
            tracing::debug!("HTTP/3 response failed: {err}");
            return;
        }
    }
    let _ = responder.finish().await;
}

/// Sends a part of a response's body, or its trailers
async fn send(
    responder: &mut Responder,
    frame: http_body::Frame<Bytes>,
) -> Result<(), h3::error::StreamError> {
    match frame.into_data() {
        Ok(data) => responder.send_data(data).await,
        Err(frame) => match frame.into_trailers() {
            Ok(trailers) => responder.send_trailers(trailers).await,
            Err(_) => Ok(()),
        },
    }
}
