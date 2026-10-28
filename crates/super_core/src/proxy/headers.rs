//! What changes between a client's request (or an upstream's response) and the one sent on:
//! the headers of a connection stay with it

use http::{header, HeaderMap, HeaderName, Uri, Version};

use crate::http::{RequestHeader, ResponseHeader};

/// Headers of a connection, never passed on (RFC 9110, 7.6.1)
const HOP_BY_HOP: [HeaderName; 5] = [
    header::CONNECTION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

const KEEP_ALIVE: &str = "keep-alive";
const PROXY_CONNECTION: &str = "proxy-connection";

/// Removes the headers of the connection; those of an upgrade are kept when `upgrade`
fn remove_hop_by_hop(headers: &mut HeaderMap, upgrade: bool) {
    // Headers the `Connection` header names are the connection's too
    if !upgrade {
        let named = headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            // Removed anyway, or not a header (`close`): no name is built for them, so the usual
            // `Connection: keep-alive` costs nothing
            .filter(|name| !name.is_empty() && !removed_anyway(name))
            .filter_map(|name| HeaderName::from_bytes(name.as_bytes()).ok())
            .collect::<Vec<_>>();
        for name in named {
            headers.remove(name);
        }
    }
    // `TE: trailers` goes on (gRPC servers need it), as with Pingora
    let trailers = headers
        .get(header::TE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"trailers"));
    for name in &HOP_BY_HOP {
        let kept = (upgrade && (name == header::CONNECTION || name == header::UPGRADE))
            || (trailers && name == header::TE);
        if !kept {
            headers.remove(name);
        }
    }
    headers.remove(KEEP_ALIVE);
    headers.remove(PROXY_CONNECTION);
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(name)
}

/// A token of `Connection` that names no other header to remove: the connection's own headers
/// (removed with them), or an option (`close`)
fn removed_anyway(token: &str) -> bool {
    token.eq_ignore_ascii_case("close")
        || token.eq_ignore_ascii_case(KEEP_ALIVE)
        || token.eq_ignore_ascii_case(PROXY_CONNECTION)
        || HOP_BY_HOP
            .iter()
            .any(|name| token.eq_ignore_ascii_case(name.as_str()))
}

/// The request sent to an upstream: `request`, without its connection's headers, for
/// HTTP/1.1 (`http2`: for HTTP/2)
pub(crate) fn upstream_request(
    request: &mut RequestHeader,
    upgrade: bool,
    http2: bool,
) -> RequestHeader {
    // What hyper parsed with the request (the case of its header names) goes to the upstream's:
    // moved, not copied
    let extensions = std::mem::take(&mut request.extensions);
    let mut upstream = request.clone();
    upstream.extensions = extensions;
    if upgrade || has_hop_by_hop(&upstream.headers) {
        remove_hop_by_hop(&mut upstream.headers, upgrade);
    }
    join_cookies(&mut upstream.headers);
    // The phases see one form, whatever the client sent and the upstream speaks: the host in
    // `Host`, and a path (`set_target` gives HTTP/2 upstreams theirs once they ran)
    set_target(&mut upstream, false);
    if http2 {
        upstream.set_version(Version::HTTP_2);
    }
    upstream
}

/// Whether a header of the connection is there: one look at each header, rather than a lookup
/// of each name (they're rarely there)
fn has_hop_by_hop(headers: &HeaderMap) -> bool {
    headers.keys().any(|name| {
        is_hop_by_hop(name) || name.as_str() == KEEP_ALIVE || name.as_str() == PROXY_CONNECTION
    })
}

/// The request's target, as its version wants it: a path (HTTP/1.1, with the host in `Host`),
/// or the whole URI (HTTP/2)
pub fn set_target(request: &mut RequestHeader, http2: bool) {
    if http2 {
        http2_target(request);
        return;
    }
    request.set_version(Version::HTTP_11);
    let Some(authority) = request.uri.authority().cloned() else {
        return;
    };
    // An HTTP/2 client's URI: its host goes to `Host`
    if !request.headers.contains_key(header::HOST) {
        if let Ok(host) = http::HeaderValue::from_str(authority.as_str()) {
            request.headers.insert(header::HOST, host);
        }
    }
    let path = request
        .uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    request.set_uri(Uri::from(path));
}

/// The whole URI, as HTTP/2 wants it: the host of `Host` (the client's, or the one a phase set)
/// as `:authority`, over TLS (HTTP/2 upstreams are reached with ALPN)
fn http2_target(request: &mut RequestHeader) {
    request.set_version(Version::HTTP_2);
    let host = request
        .headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .or_else(|| request.uri.authority().map(http::uri::Authority::as_str))
        .unwrap_or("localhost");
    let path = request
        .uri
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    let uri = Uri::builder()
        .scheme("https")
        .authority(host)
        .path_and_query(path)
        .build();
    if let Ok(uri) = uri {
        request.set_uri(uri);
    }
    request.headers.remove(header::HOST);
}

/// The upstream's response, without its connection's headers (`upgrade`: a `101`)
pub(crate) fn downstream_response(response: &mut ResponseHeader, upgrade: bool) {
    if upgrade || has_hop_by_hop(&response.headers) {
        remove_hop_by_hop(&mut response.headers, upgrade);
    }
    // They have no content, nor its length (RFC 9110 §8.6), whatever the upstream sent
    if response.status.is_informational() || response.status == http::StatusCode::NO_CONTENT {
        response.headers.remove(header::CONTENT_LENGTH);
    }
}

/// `Cookie` in one field: HTTP/2 and HTTP/3 clients may split it in several (crumbs), which an
/// HTTP/1.1 upstream must get joined by `; ` (RFC 9113 §8.2.3, RFC 9114 §4.2.1)
fn join_cookies(headers: &mut HeaderMap) {
    let mut crumbs = headers.get_all(header::COOKIE).iter();
    if crumbs.next().is_none() || crumbs.next().is_none() {
        return;
    }
    let joined = headers
        .get_all(header::COOKIE)
        .iter()
        .map(http::HeaderValue::as_bytes)
        .collect::<Vec<_>>()
        .join(&b"; "[..]);
    if let Ok(joined) = http::HeaderValue::from_bytes(&joined) {
        headers.insert(header::COOKIE, joined);
    }
}

#[cfg(test)]
mod tests {
    use http::header;

    use super::{downstream_response, set_target, upstream_request};
    use crate::http::{RequestHeader, ResponseHeader};

    #[test]
    fn test_connection_headers_stay_with_their_connection() {
        let mut request = RequestHeader::build("GET", b"/a?b", None).unwrap();
        request.insert_header(header::HOST, "a.test").unwrap();
        request
            .insert_header(header::CONNECTION, "keep-alive, x-private")
            .unwrap();
        request.insert_header("x-private", "1").unwrap();
        request.insert_header("keep-alive", "timeout=5").unwrap();
        request.insert_header("x-kept", "1").unwrap();

        let upstream = upstream_request(&mut request, false, false);
        assert!(!upstream.headers.contains_key(header::CONNECTION));
        assert!(!upstream.headers.contains_key("x-private"));
        assert!(!upstream.headers.contains_key("keep-alive"));
        assert_eq!(upstream.headers["x-kept"], "1");
        assert_eq!(upstream.uri, "/a?b");
    }

    #[test]
    fn test_usual_connection_headers_cost_nothing() {
        let mut response = ResponseHeader::build(200, None).unwrap();
        response
            .insert_header(header::CONNECTION, "Keep-Alive, close")
            .unwrap();
        response.insert_header("keep-alive", "timeout=5").unwrap();

        crate::allocations::assert_no_allocations("an upstream's Connection: keep-alive", || {
            super::remove_hop_by_hop(&mut response.headers, false);
        });
        assert!(!response.headers.contains_key(header::CONNECTION));
        assert!(!response.headers.contains_key("keep-alive"));
    }

    #[test]
    fn test_grpc_trailers_go_on() {
        let mut request = RequestHeader::build("POST", b"/svc/Method", None).unwrap();
        request.insert_header(header::TE, "trailers").unwrap();
        let upstream = upstream_request(&mut request, false, true);
        assert_eq!(upstream.headers[header::TE], "trailers");

        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request.insert_header(header::TE, "gzip").unwrap();
        assert!(!upstream_request(&mut request, false, false)
            .headers
            .contains_key(header::TE));
    }

    #[test]
    fn test_upgrades_keep_their_headers() {
        let mut request = RequestHeader::build("GET", b"/ws", None).unwrap();
        request
            .insert_header(header::CONNECTION, "Upgrade")
            .unwrap();
        request.insert_header(header::UPGRADE, "websocket").unwrap();
        let upstream = upstream_request(&mut request, true, false);
        assert_eq!(upstream.headers[header::UPGRADE], "websocket");
        assert_eq!(upstream.headers[header::CONNECTION], "Upgrade");
    }

    #[test]
    fn test_http2_clients_and_upstreams() {
        // An HTTP/2 client's request, to an HTTP/1.1 upstream
        let mut request = RequestHeader::build("GET", b"https://a.test/p?q", None).unwrap();
        let upstream = upstream_request(&mut request, false, false);
        assert_eq!(upstream.uri, "/p?q");
        assert_eq!(upstream.headers[header::HOST], "a.test");

        // An HTTP/1.1 client's request, to an HTTP/2 upstream: the phases see a path and `Host`,
        // the upstream an authority
        let mut request = RequestHeader::build("GET", b"/p", None).unwrap();
        request.insert_header(header::HOST, "b.test").unwrap();
        let mut upstream = upstream_request(&mut request, false, true);
        assert_eq!(upstream.uri, "/p");
        assert_eq!(upstream.headers[header::HOST], "b.test");
        set_target(&mut upstream, true);
        assert_eq!(upstream.uri, "https://b.test/p");
        assert!(!upstream.headers.contains_key(header::HOST));

        // An HTTP/2 client's request, to an HTTP/2 upstream: the same
        let mut request = RequestHeader::build("GET", b"https://a.test/p", None).unwrap();
        let mut upstream = upstream_request(&mut request, false, true);
        assert_eq!(upstream.uri, "/p");
        assert_eq!(upstream.headers[header::HOST], "a.test");

        // A phase's path (a rewrite) keeps the client's host...
        upstream.set_uri(http::Uri::from_static("/rewritten"));
        set_target(&mut upstream, true);
        assert_eq!(upstream.uri, "https://a.test/rewritten");

        // ...and the `Host` a phase sets (an external service's) is the authority
        let mut upstream = upstream_request(&mut request, false, true);
        upstream
            .insert_header(header::HOST, "service.test")
            .unwrap();
        set_target(&mut upstream, true);
        assert_eq!(upstream.uri, "https://service.test/p");

        let mut response = ResponseHeader::build(200, None).unwrap();
        response
            .insert_header(header::TRANSFER_ENCODING, "chunked")
            .unwrap();
        downstream_response(&mut response, false);
        assert!(!response.headers.contains_key(header::TRANSFER_ENCODING));
    }
}
