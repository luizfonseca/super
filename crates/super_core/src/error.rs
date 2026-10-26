//! Errors of the request phases: what went wrong ([`ErrorType`]), on which side
//! ([`ErrorSource`]), and whether the request can be tried again ([`RetryType`]), as Pingora's

use std::{borrow::Cow, fmt};

/// The errors of the request phases are boxed: they're moved around more than they're created
pub type Result<T, E = Box<Error>> = std::result::Result<T, E>;

/// Where an error came from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    /// The upstream (connecting to it, reading its response)
    Upstream,
    /// The client
    Downstream,
    /// Super itself
    Internal,
    Unset,
}

impl ErrorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorSource::Upstream => "Upstream",
            ErrorSource::Downstream => "Downstream",
            ErrorSource::Internal => "Internal",
            ErrorSource::Unset => "",
        }
    }
}

/// Whether a request that failed can be sent again
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryType {
    Decided(bool),
    /// Only if the connection was a reused one (it may have been closed by the upstream)
    ReusedOnly,
}

impl RetryType {
    /// Decides, knowing whether the connection was reused
    pub fn decide_reuse(&mut self, reused: bool) {
        if matches!(self, RetryType::ReusedOnly) {
            *self = RetryType::Decided(reused);
        }
    }

    pub fn retry(self) -> bool {
        matches!(self, RetryType::Decided(true))
    }
}

impl From<bool> for RetryType {
    fn from(retry: bool) -> Self {
        RetryType::Decided(retry)
    }
}

/// What went wrong
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorType {
    // Connecting to upstreams
    ConnectTimedout,
    ConnectRefused,
    ConnectNoRoute,
    TLSHandshakeFailure,
    TLSHandshakeTimedout,
    InvalidCert,
    HandshakeError,
    ConnectError,
    BindError,
    AcceptError,
    SocketError,
    // HTTP
    InvalidHTTPHeader,
    H1Error,
    H2Error,
    // Reading and writing
    ReadError,
    WriteError,
    ReadTimedout,
    WriteTimedout,
    ConnectionClosed,
    /// An answer with this status
    HTTPStatus(u16),
    // Files
    FileOpenError,
    FileCreateError,
    FileReadError,
    FileWriteError,
    InternalError,
    UnknownError,
    Custom(&'static str),
}

impl ErrorType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorType::ConnectTimedout => "ConnectTimedout",
            ErrorType::ConnectRefused => "ConnectRefused",
            ErrorType::ConnectNoRoute => "ConnectNoRoute",
            ErrorType::TLSHandshakeFailure => "TLSHandshakeFailure",
            ErrorType::TLSHandshakeTimedout => "TLSHandshakeTimedout",
            ErrorType::InvalidCert => "InvalidCert",
            ErrorType::HandshakeError => "HandshakeError",
            ErrorType::ConnectError => "ConnectError",
            ErrorType::BindError => "BindError",
            ErrorType::AcceptError => "AcceptError",
            ErrorType::SocketError => "SocketError",
            ErrorType::InvalidHTTPHeader => "InvalidHTTPHeader",
            ErrorType::H1Error => "H1Error",
            ErrorType::H2Error => "H2Error",
            ErrorType::ReadError => "ReadError",
            ErrorType::WriteError => "WriteError",
            ErrorType::ReadTimedout => "ReadTimedout",
            ErrorType::WriteTimedout => "WriteTimedout",
            ErrorType::ConnectionClosed => "ConnectionClosed",
            ErrorType::HTTPStatus(_) => "HTTPStatus",
            ErrorType::FileOpenError => "FileOpenError",
            ErrorType::FileCreateError => "FileCreateError",
            ErrorType::FileReadError => "FileReadError",
            ErrorType::FileWriteError => "FileWriteError",
            ErrorType::InternalError => "InternalError",
            ErrorType::UnknownError => "UnknownError",
            ErrorType::Custom(name) => name,
        }
    }
}

/// The error of a request phase
pub struct Error {
    pub etype: ErrorType,
    pub esource: ErrorSource,
    pub retry: RetryType,
    /// What caused it, if it came from another error
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    pub context: Option<Cow<'static, str>>,
}

impl Error {
    fn create(
        etype: ErrorType,
        esource: ErrorSource,
        context: Option<Cow<'static, str>>,
        cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Box<Self> {
        Box::new(Error {
            etype,
            esource,
            retry: RetryType::Decided(false),
            cause,
            context,
        })
    }

    pub fn new(etype: ErrorType) -> Box<Self> {
        Self::create(etype, ErrorSource::Unset, None, None)
    }

    /// An internal error, of its own kind
    pub fn new_str(name: &'static str) -> Box<Self> {
        Self::new(ErrorType::Custom(name))
    }

    /// An error, saying why
    pub fn explain(etype: ErrorType, context: impl Into<Cow<'static, str>>) -> Box<Self> {
        Self::create(etype, ErrorSource::Unset, Some(context.into()), None)
    }

    /// An error caused by another
    pub fn because(
        etype: ErrorType,
        context: impl Into<Cow<'static, str>>,
        cause: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Box<Self> {
        Self::create(
            etype,
            ErrorSource::Unset,
            Some(context.into()),
            Some(cause.into()),
        )
    }

    pub fn new_up(etype: ErrorType) -> Box<Self> {
        Self::new(etype).into_up()
    }

    pub fn new_down(etype: ErrorType) -> Box<Self> {
        Self::new(etype).into_down()
    }

    pub fn new_in(etype: ErrorType) -> Box<Self> {
        Self::new(etype).into_in()
    }

    pub fn etype(&self) -> &ErrorType {
        &self.etype
    }

    pub fn esource(&self) -> &ErrorSource {
        &self.esource
    }

    pub fn retry(&self) -> bool {
        self.retry.retry()
    }

    pub fn set_retry(&mut self, retry: bool) {
        self.retry = RetryType::Decided(retry);
    }

    /// The error, from the upstream
    #[must_use]
    pub fn into_up(mut self: Box<Self>) -> Box<Self> {
        self.esource = ErrorSource::Upstream;
        self
    }

    /// The error, from the client
    #[must_use]
    pub fn into_down(mut self: Box<Self>) -> Box<Self> {
        self.esource = ErrorSource::Downstream;
        self
    }

    /// The error, from Super itself
    #[must_use]
    pub fn into_in(mut self: Box<Self>) -> Box<Self> {
        self.esource = ErrorSource::Internal;
        self
    }

    /// The error, with more context before what it says
    #[must_use]
    pub fn more_context(mut self: Box<Self>, context: impl Into<Cow<'static, str>>) -> Box<Self> {
        let context = context.into();
        self.context = Some(match self.context.take() {
            Some(previous) => format!("{context}: {previous}").into(),
            None => context,
        });
        self
    }

    /// The first error of `ErrorType` among the causes, this one included
    pub fn root_etype(&self) -> &ErrorType {
        // A cause given boxed is boxed again
        let cause = self.cause.as_ref().and_then(|cause| {
            cause
                .downcast_ref::<Error>()
                .or_else(|| cause.downcast_ref::<Box<Error>>().map(AsRef::as_ref))
        });
        cause.map_or(&self.etype, Error::root_etype)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = self.esource.as_str();
        if !source.is_empty() {
            write!(f, "{source} ")?;
        }
        write!(f, "{}", self.etype.as_str())?;
        if let ErrorType::HTTPStatus(code) = self.etype {
            write!(f, " {code}")?;
        }
        if let Some(context) = &self.context {
            write!(f, " context: {context}")?;
        }
        if let Some(cause) = &self.cause {
            write!(f, " cause: {cause}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// Turns other errors into the errors of the request phases
pub trait OrErr<T, E> {
    /// The error, of `etype`, caused by the one returned
    ///
    /// # Errors
    ///
    /// When `self` is an error.
    fn or_err(self, etype: ErrorType, context: &'static str) -> Result<T>;
}

impl<T, E: Into<Box<dyn std::error::Error + Send + Sync>>> OrErr<T, E>
    for std::result::Result<T, E>
{
    fn or_err(self, etype: ErrorType, context: &'static str) -> Result<T> {
        self.map_err(|err| Error::because(etype, context, err))
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, ErrorSource, ErrorType, RetryType};

    #[test]
    fn test_errors_read_as_pingora_s() {
        let error =
            Error::explain(ErrorType::HTTPStatus(502), "the upstream answered 502").into_up();
        assert_eq!(
            error.to_string(),
            "Upstream HTTPStatus 502 context: the upstream answered 502"
        );
        assert_eq!(*error.esource(), ErrorSource::Upstream);

        let error = error.more_context("Peer: 10.0.0.1:80");
        assert_eq!(
            error.context.as_deref(),
            Some("Peer: 10.0.0.1:80: the upstream answered 502")
        );
    }

    #[test]
    fn test_retries_of_reused_connections_only() {
        let mut retry = RetryType::ReusedOnly;
        assert!(!retry.retry());
        retry.decide_reuse(true);
        assert!(retry.retry());

        let mut retry = RetryType::ReusedOnly;
        retry.decide_reuse(false);
        assert!(!retry.retry());
    }

    #[test]
    fn test_root_error_type() {
        let root = Error::new(ErrorType::ConnectRefused);
        let error = Error::because(ErrorType::ConnectError, "connecting", root);
        assert_eq!(*error.root_etype(), ErrorType::ConnectRefused);
    }
}
