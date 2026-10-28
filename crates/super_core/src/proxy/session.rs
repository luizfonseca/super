//! A request being proxied: the client's session, the request's cache state and modules, and
//! what's kept of its body to send it again

use std::ops::{Deref, DerefMut};

use bytes::Bytes;

use crate::{
    cache::HttpCache, http::ResponseHeader, modules::ModulesCtx, protocols::http::ServerSession,
    Result,
};

/// Parts of a request body kept to send it again, at most (Pingora's limit)
pub(crate) const RETRY_BUFFER_LIMIT: usize = 64 * 1024;

/// The first parts of a request body, as sent to the upstream, to send them again
#[derive(Default)]
pub(crate) struct RetryBuffer {
    pub(crate) parts: Vec<Bytes>,
    pub(crate) size: usize,
    /// Larger than the limit: the body can't be sent again
    pub(crate) truncated: bool,
}

impl RetryBuffer {
    pub(crate) fn keep(&mut self, part: &Bytes) {
        if self.truncated {
            return;
        }
        self.size += part.len();
        if self.size > RETRY_BUFFER_LIMIT {
            self.truncated = true;
            self.parts = Vec::new();
            return;
        }
        self.parts.push(part.clone());
    }
}

/// A request being proxied
pub struct Session {
    pub downstream_session: ServerSession,
    pub cache: HttpCache,
    /// What the response goes through (its compression)
    pub downstream_modules_ctx: ModulesCtx,
    pub(crate) retry_buffer: RetryBuffer,
    /// Whether the whole request body was sent to an upstream
    pub(crate) request_body_sent: bool,
}

impl Session {
    pub fn new(downstream_session: ServerSession) -> Self {
        Self {
            downstream_session,
            cache: HttpCache::new(),
            downstream_modules_ctx: ModulesCtx::default(),
            retry_buffer: RetryBuffer::default(),
            request_body_sent: false,
        }
    }

    /// Writes the response's header (through the modules), and ends it with `end_of_stream`
    ///
    /// # Errors
    ///
    /// When the client is gone, or a response was already written.
    pub async fn write_response_header(
        &mut self,
        mut header: ResponseHeader,
        end_of_stream: bool,
    ) -> Result<()> {
        self.downstream_modules_ctx
            .compression()
            .response_header_filter(&mut header, end_of_stream);
        self.downstream_session
            .write_response_header(header)
            .await?;
        if end_of_stream {
            self.downstream_session.finish_body().await?;
        }
        Ok(())
    }

    /// # Errors
    ///
    /// When the client is gone, or a response was already written.
    pub async fn write_response_header_ref(
        &mut self,
        header: &ResponseHeader,
        end_of_stream: bool,
    ) -> Result<()> {
        self.write_response_header(header.clone(), end_of_stream)
            .await
    }

    /// Writes a part of the response body (through the modules), the last one with `end`
    ///
    /// # Errors
    ///
    /// When the client is gone, or too slow.
    pub async fn write_response_body(&mut self, data: Option<Bytes>, end: bool) -> Result<()> {
        let compression = self.downstream_modules_ctx.compression();
        let data = match compression.response_body_filter(data.as_ref(), end) {
            Some(compressed) => Some(compressed),
            None => data,
        };
        match data {
            Some(data) if !data.is_empty() || end => {
                self.downstream_session.write_response_body(data, end).await
            }
            _ if end => self.downstream_session.finish_body().await,
            _ => Ok(()),
        }
    }

    /// Whether the request body is too large to be sent again (when the request is retried)
    pub fn retry_buffer_truncated(&self) -> bool {
        self.retry_buffer.truncated
    }

    pub fn as_downstream(&self) -> &ServerSession {
        &self.downstream_session
    }

    pub fn as_downstream_mut(&mut self) -> &mut ServerSession {
        &mut self.downstream_session
    }
}

impl Deref for Session {
    type Target = ServerSession;

    fn deref(&self) -> &Self::Target {
        &self.downstream_session
    }
}

impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.downstream_session
    }
}

impl AsRef<ServerSession> for Session {
    fn as_ref(&self) -> &ServerSession {
        &self.downstream_session
    }
}
