//! The headers of requests and responses, as the request phases change them: [`http`]'s parts,
//! with the helpers of Pingora's (`insert_header`, `append_header`...)

use std::ops::{Deref, DerefMut};

use http::{
    header::{AsHeaderName, HeaderName, HeaderValue},
    request, response, Method, StatusCode, Uri, Version,
};

use crate::{Error, ErrorType, OrErr, Result};

/// A header name: a [`HeaderName`], or a string parsed into one
pub trait IntoHeaderName {
    /// # Errors
    ///
    /// When it isn't a valid header name.
    fn into_header_name(self) -> Result<HeaderName>;
}

impl IntoHeaderName for HeaderName {
    fn into_header_name(self) -> Result<HeaderName> {
        Ok(self)
    }
}

impl IntoHeaderName for &HeaderName {
    fn into_header_name(self) -> Result<HeaderName> {
        Ok(self.clone())
    }
}

impl IntoHeaderName for &str {
    fn into_header_name(self) -> Result<HeaderName> {
        HeaderName::from_bytes(self.as_bytes())
            .or_err(ErrorType::InvalidHTTPHeader, "invalid header name")
    }
}

impl IntoHeaderName for bytes::Bytes {
    fn into_header_name(self) -> Result<HeaderName> {
        HeaderName::from_bytes(&self).or_err(ErrorType::InvalidHTTPHeader, "invalid header name")
    }
}

impl IntoHeaderName for String {
    fn into_header_name(self) -> Result<HeaderName> {
        self.as_str().into_header_name()
    }
}

fn header_value<V>(value: V) -> Result<HeaderValue>
where
    V: TryInto<HeaderValue>,
    V::Error: std::error::Error + Send + Sync + 'static,
{
    value
        .try_into()
        .or_err(ErrorType::InvalidHTTPHeader, "invalid header value")
}

/// The header of a request: its method, URI, version and headers
#[derive(Debug, Clone)]
pub struct RequestHeader {
    parts: request::Parts,
}

impl RequestHeader {
    /// A request for `path` (and its query), as HTTP/1.1
    ///
    /// # Errors
    ///
    /// When the method or the path aren't valid.
    pub fn build(
        method: impl TryInto<Method>,
        path: &[u8],
        _size_hint: Option<usize>,
    ) -> Result<Self> {
        let method = method
            .try_into()
            .map_err(|_| Error::explain(ErrorType::InvalidHTTPHeader, "invalid method"))?;
        let uri = Uri::try_from(path).or_err(ErrorType::InvalidHTTPHeader, "invalid path")?;
        let (mut parts, ()) = http::Request::new(()).into_parts();
        parts.method = method;
        parts.uri = uri;
        parts.version = Version::HTTP_11;
        Ok(Self { parts })
    }

    /// Like [`Self::build`] (header names are always sent lowercase)
    ///
    /// # Errors
    ///
    /// When the method or the path aren't valid.
    pub fn build_no_case(
        method: impl TryInto<Method>,
        path: &[u8],
        size_hint: Option<usize>,
    ) -> Result<Self> {
        Self::build(method, path, size_hint)
    }

    pub fn from_parts(parts: request::Parts) -> Self {
        Self { parts }
    }

    pub fn into_parts(self) -> request::Parts {
        self.parts
    }

    pub fn as_parts(&self) -> &request::Parts {
        &self.parts
    }

    /// Sets `name` to `value`, replacing its values
    ///
    /// # Errors
    ///
    /// When the name or the value isn't valid.
    pub fn insert_header<V>(&mut self, name: impl IntoHeaderName, value: V) -> Result<()>
    where
        V: TryInto<HeaderValue>,
        V::Error: std::error::Error + Send + Sync + 'static,
    {
        self.parts
            .headers
            .insert(name.into_header_name()?, header_value(value)?);
        Ok(())
    }

    /// Adds `value` to the values of `name`. Returns whether it had values already.
    ///
    /// # Errors
    ///
    /// When the name or the value isn't valid.
    pub fn append_header<V>(&mut self, name: impl IntoHeaderName, value: V) -> Result<bool>
    where
        V: TryInto<HeaderValue>,
        V::Error: std::error::Error + Send + Sync + 'static,
    {
        Ok(self
            .parts
            .headers
            .append(name.into_header_name()?, header_value(value)?))
    }

    /// Removes `name`, returning its first value
    pub fn remove_header<K: AsHeaderName>(&mut self, name: K) -> Option<HeaderValue> {
        self.parts.headers.remove(name)
    }

    pub fn set_uri(&mut self, uri: Uri) {
        self.parts.uri = uri;
    }

    pub fn set_method(&mut self, method: Method) {
        self.parts.method = method;
    }

    pub fn set_version(&mut self, version: Version) {
        self.parts.version = version;
    }

    /// The path and query the request is for
    pub fn raw_path(&self) -> &[u8] {
        self.parts
            .uri
            .path_and_query()
            .map_or(b"/", |path| path.as_str().as_bytes())
    }
}

impl Deref for RequestHeader {
    type Target = request::Parts;

    fn deref(&self) -> &Self::Target {
        &self.parts
    }
}

impl DerefMut for RequestHeader {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.parts
    }
}

/// The header of a response: its status, version and headers
#[derive(Debug, Clone)]
pub struct ResponseHeader {
    parts: response::Parts,
}

impl ResponseHeader {
    /// A response with `code`
    ///
    /// # Errors
    ///
    /// When `code` isn't a valid status.
    pub fn build(code: impl TryInto<StatusCode>, _size_hint: Option<usize>) -> Result<Self> {
        let status = code
            .try_into()
            .map_err(|_| Error::explain(ErrorType::InvalidHTTPHeader, "invalid status"))?;
        let (mut parts, ()) = http::Response::new(()).into_parts();
        parts.status = status;
        Ok(Self { parts })
    }

    /// Like [`Self::build`] (header names are always sent lowercase)
    ///
    /// # Errors
    ///
    /// When `code` isn't a valid status.
    pub fn build_no_case(code: impl TryInto<StatusCode>, size_hint: Option<usize>) -> Result<Self> {
        Self::build(code, size_hint)
    }

    pub fn from_parts(parts: response::Parts) -> Self {
        Self { parts }
    }

    pub fn into_parts(self) -> response::Parts {
        self.parts
    }

    pub fn as_parts(&self) -> &response::Parts {
        &self.parts
    }

    /// Sets `name` to `value`, replacing its values
    ///
    /// # Errors
    ///
    /// When the name or the value isn't valid.
    pub fn insert_header<V>(&mut self, name: impl IntoHeaderName, value: V) -> Result<()>
    where
        V: TryInto<HeaderValue>,
        V::Error: std::error::Error + Send + Sync + 'static,
    {
        self.parts
            .headers
            .insert(name.into_header_name()?, header_value(value)?);
        Ok(())
    }

    /// Adds `value` to the values of `name`. Returns whether it had values already.
    ///
    /// # Errors
    ///
    /// When the name or the value isn't valid.
    pub fn append_header<V>(&mut self, name: impl IntoHeaderName, value: V) -> Result<bool>
    where
        V: TryInto<HeaderValue>,
        V::Error: std::error::Error + Send + Sync + 'static,
    {
        Ok(self
            .parts
            .headers
            .append(name.into_header_name()?, header_value(value)?))
    }

    /// Removes `name`, returning its first value
    pub fn remove_header<K: AsHeaderName>(&mut self, name: K) -> Option<HeaderValue> {
        self.parts.headers.remove(name)
    }

    /// # Errors
    ///
    /// When `code` isn't a valid status.
    pub fn set_status(&mut self, code: impl TryInto<StatusCode>) -> Result<()> {
        self.parts.status = code
            .try_into()
            .map_err(|_| Error::explain(ErrorType::InvalidHTTPHeader, "invalid status"))?;
        Ok(())
    }
}

impl Deref for ResponseHeader {
    type Target = response::Parts;

    fn deref(&self) -> &Self::Target {
        &self.parts
    }
}

impl DerefMut for ResponseHeader {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.parts
    }
}

#[cfg(test)]
mod tests {
    use http::header;

    use super::{RequestHeader, ResponseHeader};

    #[test]
    fn test_headers_by_any_name() {
        let mut response = ResponseHeader::build(200, None).unwrap();
        response
            .insert_header(header::CONTENT_LENGTH, 10usize)
            .unwrap();
        response.insert_header("X-Custom", "a").unwrap();
        assert!(response.append_header("x-custom".to_string(), "b").unwrap());

        assert_eq!(response.headers[header::CONTENT_LENGTH], "10");
        assert_eq!(response.headers.get_all("x-custom").iter().count(), 2);
        assert!(response.insert_header("bad name", "a").is_err());
        assert!(response.insert_header("x-bad", "a\nb").is_err());
    }

    #[test]
    fn test_requests_built_for_a_path() {
        let mut request = RequestHeader::build("POST", b"/a?b=c", None).unwrap();
        assert_eq!(request.method, http::Method::POST);
        assert_eq!(request.uri.path(), "/a");
        assert_eq!(request.raw_path(), b"/a?b=c");

        request.set_uri("/d".parse().unwrap());
        assert_eq!(request.raw_path(), b"/d");
        assert!(RequestHeader::build("GET", b"not a path", None).is_err());
    }
}
