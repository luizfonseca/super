//! What an intermediary owes the requests it forwards (RFC 9110 §7.6): `Max-Forwards`, which an
//! `OPTIONS` or a `TRACE` reaching `0` here is answered at, and `Via`, which says who forwarded
//! the request, over which version of HTTP

use http::{header, HeaderValue, Method, StatusCode, Version};
use super_core::{
    http::{RequestHeader, ResponseHeader},
    proxy::Session,
};

/// `Max-Forwards`, for the methods it applies to (`OPTIONS` and `TRACE`)
fn max_forwards(request: &RequestHeader) -> Option<u32> {
    if request.method != Method::OPTIONS && request.method != Method::TRACE {
        return None;
    }
    request
        .headers
        .get(header::MAX_FORWARDS)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// Answers an `OPTIONS` or a `TRACE` that can't be forwarded further (`Max-Forwards: 0`), as its
/// final recipient: `OPTIONS` without anything more to say, `TRACE` refused (echoing requests
/// would show their cookies to scripts). `true` once answered.
///
/// # Errors
///
/// When the response can't be written.
pub async fn answer_last_hop(session: &mut Session) -> super_core::Result<bool> {
    if max_forwards(session.req_header()) != Some(0) {
        return Ok(false);
    }
    let status = if session.req_header().method == Method::TRACE {
        StatusCode::METHOD_NOT_ALLOWED
    } else {
        StatusCode::OK
    };
    let mut response = ResponseHeader::build(status, Some(1))?;
    response.insert_header(header::CONTENT_LENGTH, HeaderValue::from_static("0"))?;
    session
        .write_response_header(Box::new(response), true)
        .await?;
    Ok(true)
}

/// `Max-Forwards` decremented, and `Via` added, in the request sent upstream
pub fn forward(client: &RequestHeader, upstream: &mut RequestHeader) {
    if let Some(left) = max_forwards(client).and_then(|left| left.checked_sub(1)) {
        upstream
            .headers
            .insert(header::MAX_FORWARDS, HeaderValue::from(left));
    }
    let via = HeaderValue::from_static(match client.version {
        Version::HTTP_09 => "0.9 super",
        Version::HTTP_10 => "1.0 super",
        Version::HTTP_2 => "2 super",
        Version::HTTP_3 => "3 super",
        _ => "1.1 super",
    });
    // After the proxies the request went through
    let joined = upstream.headers.get(header::VIA).and_then(|earlier| {
        let earlier = earlier.to_str().ok()?;
        HeaderValue::from_str(&format!("{earlier}, {}", via.to_str().ok()?)).ok()
    });
    upstream.headers.insert(header::VIA, joined.unwrap_or(via));
}

#[cfg(test)]
mod tests {
    use http::{header, Version};
    use super_core::http::RequestHeader;

    use super::forward;

    #[test]
    fn test_via_and_max_forwards() {
        let mut client = RequestHeader::build("OPTIONS", b"/", None).unwrap();
        client.insert_header(header::MAX_FORWARDS, "5").unwrap();
        client.set_version(Version::HTTP_2);
        let mut upstream = client.clone();
        forward(&client, &mut upstream);
        assert_eq!(upstream.headers[header::MAX_FORWARDS], "4");
        assert_eq!(upstream.headers[header::VIA], "2 super");

        // After the proxies it went through; Max-Forwards is only for OPTIONS and TRACE
        let mut client = RequestHeader::build("GET", b"/", None).unwrap();
        client.insert_header(header::VIA, "1.1 edge").unwrap();
        client.insert_header(header::MAX_FORWARDS, "5").unwrap();
        let mut upstream = client.clone();
        forward(&client, &mut upstream);
        assert_eq!(upstream.headers[header::VIA], "1.1 edge, 1.1 super");
        assert_eq!(upstream.headers[header::MAX_FORWARDS], "5");
    }
}
