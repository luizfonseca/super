//! One attempt at proxying a request to an upstream: the request (and its body) sent, the
//! response read through the phases, and its body streamed to the client

use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt;
use http_body_util::BodyExt;
use hyper::body::{Body, Incoming};

use super::{cache_phase, headers, tunnel, upload, HttpProxy, ProxyHttp, Session};
use crate::{
    http::ResponseHeader,
    upstreams::{
        body::UpstreamBody,
        connector::{HttpSession, Sender},
        peer::HttpPeer,
    },
    Error, ErrorType, Result,
};

/// Proxies the request on `connection`, to `peer`
pub(super) async fn proxy<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    peer: &HttpPeer,
    mut connection: HttpSession,
) -> Result<()> {
    let http2 = connection.is_http2();
    let upgrade = session.is_upgrade_req() && !http2;
    let mut request = headers::upstream_request(session.req_header_mut(), upgrade, http2);
    proxy
        .inner
        .upstream_request_filter(session, &mut request, ctx)
        .await?;
    headers::set_target(&mut request, http2);
    let options = &peer.options;
    let (mut upload, body) =
        upload::Upload::start(proxy, session, ctx, options.write_timeout).await?;
    let request = hyper::Request::from_parts(request.into_parts(), body);

    let sending = with_timeout(send(&mut connection.sender, request), options.read_timeout);
    let response = upload::alongside(sending, &mut upload, proxy, session, ctx).await??;
    if upgrade && response.status() == http::StatusCode::SWITCHING_PROTOCOLS {
        return tunnel::run(proxy, session, ctx, response).await;
    }

    let (parts, mut body) = response.into_parts();
    let mut header = ResponseHeader::from_parts(parts);
    headers::downstream_response(&mut header, false);
    proxy
        .inner
        .upstream_response_filter(session, &mut header, ctx)
        .await?;
    cache_phase::admit(proxy, session, ctx, &header).await;
    proxy
        .inner
        .response_filter(session, &mut header, ctx)
        .await?;
    let head = session.req_header().method == http::Method::HEAD;
    let empty = head || body.is_end_stream();
    session
        .write_response_header(Box::new(header), false)
        .await?;

    if empty {
        respond_body(proxy, session, ctx, None, true).await?;
    } else {
        stream(
            proxy,
            session,
            ctx,
            &mut body,
            &mut upload,
            options.read_timeout,
        )
        .await?;
    }
    // The upstream answered before the whole request body was sent: the client's rest is read
    upload::drain(&upload, session).await;
    // Kept for the next request once both bodies are whole
    if upload.is_done() {
        proxy
            .connector
            .release_http_session(connection, options.idle_timeout);
    }
    Ok(())
}

/// Streams the upstream's response body to the client
async fn stream<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    body: &mut Incoming,
    upload: &mut upload::Upload,
    read_timeout: Option<Duration>,
) -> Result<()> {
    loop {
        // The response's header goes first when its body takes time to come
        let frame = if let Some(frame) = body.frame().now_or_never() {
            frame
        } else {
            session.downstream_session.flush_header()?;
            let next = with_timeout(body.frame().map(Ok), read_timeout);
            upload::alongside(next, upload, proxy, session, ctx).await??
        };
        let Some(frame) = frame else {
            return respond_body(proxy, session, ctx, None, true).await;
        };
        if forward(proxy, session, ctx, body, frame).await? {
            return Ok(());
        }
    }
}

/// Sends a frame of the upstream's body on to the client: `true` once the body ended
async fn forward<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    body: &Incoming,
    frame: std::result::Result<hyper::body::Frame<Bytes>, hyper::Error>,
) -> Result<bool> {
    let frame = frame.map_err(|err| {
        Error::because(ErrorType::ReadError, "reading the upstream's body", err).into_up()
    })?;
    let frame = match frame.into_data() {
        Ok(data) => {
            let end = body.is_end_stream();
            respond_body(proxy, session, ctx, Some(data), end).await?;
            return Ok(end);
        }
        Err(frame) => frame,
    };
    let Ok(trailers) = frame.into_trailers() else {
        return Ok(false);
    };
    respond_body(proxy, session, ctx, None, false).await?;
    cache_phase::write(session, None, true).await;
    session
        .downstream_session
        .write_response_trailers(trailers)
        .await?;
    Ok(true)
}

/// A part of the response body: stored in the cache, through `response_body_filter` (and its
/// pace), then sent
pub(super) async fn respond_body<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    data: Option<Bytes>,
    end: bool,
) -> Result<()> {
    cache_phase::write(session, data.as_ref(), end).await;
    let mut data = data;
    if let Some(wait) = proxy
        .inner
        .response_body_filter(session, &mut data, end, ctx)?
    {
        tokio::time::sleep(wait).await;
    }
    session.write_response_body(data, end).await
}

/// Sends `request` once the connection takes it
async fn send(
    sender: &mut Sender,
    request: hyper::Request<UpstreamBody>,
) -> Result<hyper::Response<Incoming>> {
    let closed = |err: hyper::Error| {
        let mut e =
            Error::because(ErrorType::ConnectionClosed, "sending the request", err).into_up();
        // A connection kept from a previous request may have been closed by the upstream
        e.retry = crate::RetryType::ReusedOnly;
        e
    };
    match sender {
        Sender::H1(sender) => {
            sender.ready().await.map_err(closed)?;
            sender.send_request(request).await.map_err(closed)
        }
        Sender::H2(sender, _) => {
            sender.ready().await.map_err(closed)?;
            sender.send_request(request).await.map_err(closed)
        }
    }
}

/// `future`, unless the upstream takes longer than `timeout`
async fn with_timeout<T>(
    future: impl std::future::Future<Output = Result<T>>,
    timeout: Option<Duration>,
) -> Result<T> {
    let Some(timeout) = timeout else {
        return future.await;
    };
    tokio::time::timeout(timeout, future).await.map_err(|_| {
        Error::explain(ErrorType::ReadTimedout, "waiting for the upstream").into_up()
    })?
}
