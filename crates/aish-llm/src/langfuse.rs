//! Optional Langfuse observability integration.
//!
//! Provides best-effort tracing of LLM sessions, generation spans, and tool call spans.
//! All methods are async and non-blocking — errors are logged but never propagated to callers.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the Langfuse observability client.
#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub enabled: bool,
    pub public_key: String,
    pub secret_key: String,
    pub base_url: String,
}

impl LangfuseConfig {
    /// Build a LangfuseConfig from the optional fields in the application config.
    /// Returns `None` if either key is missing (i.e. Langfuse is not configured).
    pub fn from_parts(
        public_key: Option<&str>,
        secret_key: Option<&str>,
        host: Option<&str>,
    ) -> Option<Self> {
        let public_key = public_key?.to_string();
        let secret_key = secret_key?.to_string();
        if public_key.is_empty() || secret_key.is_empty() {
            return None;
        }
        Some(Self {
            enabled: true,
            public_key,
            secret_key,
            base_url: host
                .map(|h| h.trim_end_matches('/').to_string())
                .unwrap_or_else(|| "https://cloud.langfuse.com".to_string()),
        })
    }
}

// ---------------------------------------------------------------------------
// Ingestion event types (Langfuse Ingestion API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceEvent {
    id: String,
    name: String,
    metadata: serde_json::Value,
    timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationEvent {
    id: String,
    trace_id: String,
    name: String,
    model: String,
    input: serde_json::Value,
    output: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<UsagePayload>,
    timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpanEvent {
    id: String,
    trace_id: String,
    name: String,
    input: serde_json::Value,
    output: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
    timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsagePayload {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

/// Wrapper for the batched ingestion payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestionBatch {
    batch: Vec<IngestionItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum IngestionItem {
    #[serde(rename = "trace")]
    Trace(TraceEvent),
    #[serde(rename = "generation")]
    Generation(GenerationEvent),
    #[serde(rename = "span")]
    Span(SpanEvent),
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Best-effort Langfuse client that buffers events and flushes them via the
/// Ingestion API.  All public methods swallow errors and log warnings instead.
#[derive(Debug, Clone)]
pub struct LangfuseClient {
    http: Client,
    base_url: String,
    auth_header: String,
    buffer: Arc<Mutex<Vec<IngestionItem>>>,
}

impl LangfuseClient {
    pub fn new(config: LangfuseConfig) -> Self {
        let auth_header = format!("Bearer {}:{}", config.public_key, config.secret_key);
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
            base_url: config.base_url,
            auth_header,
            buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // -- Public high-level helpers -------------------------------------------

    /// Create a trace for a session and return the trace ID.
    pub async fn trace_session(&self, session_id: &str, metadata: &serde_json::Value) -> String {
        let trace_id = Uuid::new_v4().to_string();
        let event = TraceEvent {
            id: trace_id.clone(),
            name: format!("session-{}", session_id),
            metadata: metadata.clone(),
            timestamp: now_iso(),
        };
        self.push(IngestionItem::Trace(event)).await;
        trace_id
    }

    /// Log a generation span under an existing trace.
    pub async fn span_generation(
        &self,
        trace_id: &str,
        model: &str,
        input: &str,
        output: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) {
        let gen_id = Uuid::new_v4().to_string();
        let usage = if prompt_tokens > 0 || completion_tokens > 0 {
            Some(UsagePayload {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            })
        } else {
            None
        };
        let event = GenerationEvent {
            id: gen_id,
            trace_id: trace_id.to_string(),
            name: "generation".to_string(),
            model: model.to_string(),
            input: json!(input),
            output: json!(output),
            usage,
            timestamp: now_iso(),
        };
        self.push(IngestionItem::Generation(event)).await;
    }

    /// Log a tool-call span under an existing trace.
    pub async fn span_tool_call(
        &self,
        trace_id: &str,
        tool_name: &str,
        args: &str,
        result: &str,
        duration_ms: u64,
    ) {
        let span_id = Uuid::new_v4().to_string();
        let event = SpanEvent {
            id: span_id,
            trace_id: trace_id.to_string(),
            name: format!("tool-{}", tool_name),
            input: json!(args),
            output: json!(result),
            metadata: Some(json!({ "duration_ms": duration_ms })),
            timestamp: now_iso(),
        };
        self.push(IngestionItem::Span(event)).await;
    }

    /// Flush all buffered events to the Langfuse Ingestion API.
    /// Errors are logged but not returned.
    pub async fn flush(&self) {
        let items = {
            let mut buf = self.buffer.lock().await;
            std::mem::take(&mut *buf)
        };
        if items.is_empty() {
            return;
        }

        let url = format!("{}/api/public/ingestion", self.base_url);
        let body = IngestionBatch { batch: items };

        match self
            .http
            .post(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    warn!("Langfuse ingestion failed: {} {}", status, text);
                }
            }
            Err(e) => {
                warn!("Langfuse ingestion error: {}", e);
            }
        }
    }

    // -- Internal helpers ----------------------------------------------------

    async fn push(&self, item: IngestionItem) {
        let mut buf = self.buffer.lock().await;
        buf.push(item);
        // Auto-flush when buffer reaches 20 items
        if buf.len() >= 20 {
            let items = std::mem::take(&mut *buf);
            drop(buf); // release lock before network call
            self.send_batch(items).await;
        }
    }

    async fn send_batch(&self, items: Vec<IngestionItem>) {
        let url = format!("{}/api/public/ingestion", self.base_url);
        let body = IngestionBatch { batch: items };

        if let Err(e) = self
            .http
            .post(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            warn!("Langfuse batch send error: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // ISO 8601 with millisecond precision
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    // Simple formatting without chrono dependency
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Convert epoch days to year-month-day (Gregorian calendar algorithm)
    let (year, month, day) = epoch_days_to_date(days_since_epoch);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hours, minutes, seconds, millis
    )
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant: http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m as u64, d as u64)
}
