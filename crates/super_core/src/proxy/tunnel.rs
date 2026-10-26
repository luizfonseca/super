//! A connection switching protocols (e.g. a websocket): once the upstream accepts (`101`), bytes
//! go both ways untouched until either side closes

use hyper::body::Incoming;
use hyper_util::rt::TokioIo;

use super::{headers, HttpProxy, ProxyHttp, Session};
use crate::{http::ResponseHeader, Error, ErrorType, Result};

pub(super) async fn run<SV: ProxyHttp>(
    proxy: &HttpProxy<SV>,
    session: &mut Session,
    ctx: &mut SV::CTX,
    mut response: hyper::Response<Incoming>,
) -> Result<()> {
    let upstream = hyper::upgrade::on(&mut response);
    let (parts, _) = response.into_parts();
    let mut header = ResponseHeader::from_parts(parts);
    headers::downstream_response(&mut header, true);
    proxy
        .inner
        .upstream_response_filter(session, &mut header, ctx)
        .await?;
    proxy
        .inner
        .response_filter(session, &mut header, ctx)
        .await?;
    let Some(downstream) = session.take_upgrade() else {
        return Err(Error::explain(
            ErrorType::InternalError,
            "no connection to upgrade",
        ));
    };
    session
        .write_response_header(Box::new(header), true)
        .await?;

    let upstream = upstream
        .await
        .map_err(|err| Error::because(ErrorType::ReadError, "upgrading", err).into_up())?;
    let downstream = downstream
        .await
        .map_err(|err| Error::because(ErrorType::ReadError, "upgrading", err).into_down())?;
    let (mut upstream, mut downstream) = (TokioIo::new(upstream), TokioIo::new(downstream));
    match tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await {
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::debug!("upgraded connection closed: {err}");
            Ok(())
        }
    }
}
