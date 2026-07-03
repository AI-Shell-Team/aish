use std::pin::Pin;

use aish_core::AishError;
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Response;

/// Unified reader for LLM streaming bodies (native HTTP or translated SSE).
pub struct LlmStream {
    inner: LlmStreamInner,
}

enum LlmStreamInner {
    Http(Response),
    Translated(Pin<Box<dyn futures::Stream<Item = Result<Bytes, AishError>> + Send>>),
}

impl LlmStream {
    pub fn from_http(resp: Response) -> Self {
        Self {
            inner: LlmStreamInner::Http(resp),
        }
    }

    pub fn from_translated(
        stream: Pin<Box<dyn futures::Stream<Item = Result<Bytes, AishError>> + Send>>,
    ) -> Self {
        Self {
            inner: LlmStreamInner::Translated(stream),
        }
    }

    pub async fn chunk(&mut self) -> Result<Option<Bytes>, AishError> {
        match &mut self.inner {
            LlmStreamInner::Http(resp) => resp
                .chunk()
                .await
                .map_err(|e| AishError::Llm(e.to_string())),
            LlmStreamInner::Translated(stream) => match stream.next().await {
                Some(Ok(bytes)) => Ok(Some(bytes)),
                Some(Err(err)) => Err(err),
                None => Ok(None),
            },
        }
    }
}
