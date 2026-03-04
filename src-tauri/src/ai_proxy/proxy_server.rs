//! Axum HTTP proxy server sitting between user tools and CLIProxyAPIPlus.
//!
//! Architecture: `User tools → Axum proxy (proxy_port) → CLIProxyAPIPlus (backend_port)`
//!
//! Features:
//! - Transform pipeline (thinking/reasoning suffixes, model groups)
//! - SSE streaming passthrough
//! - Automatic failover on 429/5xx for model group requests
//! - Usage tracking per model/provider

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

use super::model_groups::ModelGroupRegistry;
use super::transforms;
use super::types::{ProxyModelGroup, ProxyStatus, UsageStats};
use super::usage::UsageTracker;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ProxyState {
    backend_port: u16,
    model_groups: Arc<RwLock<ModelGroupRegistry>>,
    usage: Arc<UsageTracker>,
    client: reqwest::Client,
}

impl ProxyState {
    fn backend_url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.backend_port)
    }
}

// ---------------------------------------------------------------------------
// Server handle & global singleton
// ---------------------------------------------------------------------------

pub struct ProxyServerHandle {
    pub shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub port: u16,
    pub backend_port: u16,
}

static PROXY_HANDLE: Lazy<Mutex<Option<ProxyServerHandle>>> = Lazy::new(|| Mutex::new(None));

/// Shared state refs kept separately so `update_model_groups` / `get_usage_stats`
/// can access them without the server handle.
static SHARED_STATE: Lazy<Mutex<Option<SharedRefs>>> = Lazy::new(|| Mutex::new(None));

struct SharedRefs {
    model_groups: Arc<RwLock<ModelGroupRegistry>>,
    usage: Arc<UsageTracker>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Start the proxy server. Binds to `127.0.0.1:proxy_port`.
pub async fn start_proxy(
    proxy_port: u16,
    backend_port: u16,
    groups: Vec<ProxyModelGroup>,
) -> Result<ProxyServerHandle, String> {
    // Tear down previous instance if running
    stop_proxy();

    let model_groups = Arc::new(RwLock::new(ModelGroupRegistry::new(groups)));
    let usage = Arc::new(UsageTracker::new());

    let state = ProxyState {
        backend_port,
        model_groups: Arc::clone(&model_groups),
        usage: Arc::clone(&usage),
        client: reqwest::Client::new(),
    };

    // Store shared refs for external access
    *SHARED_STATE.lock().await = Some(SharedRefs {
        model_groups: Arc::clone(&model_groups),
        usage: Arc::clone(&usage),
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let router = Router::new()
        .route("/v1/chat/completions", post(handle_completions))
        .route("/v1/messages", post(handle_completions))
        .route("/v1/models", get(handle_models))
        .route("/proxy/status", get(handle_proxy_status))
        .route("/proxy/usage", get(handle_proxy_usage))
        .route("/proxy/usage/reset", post(handle_usage_reset))
        .fallback(any(handle_passthrough))
        .layer(cors)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], proxy_port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind proxy to port {proxy_port}: {e}"))?;

    let bound_port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {e}"))?
        .port();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        log::info!("AI proxy listening on 127.0.0.1:{bound_port} → backend :{backend_port}");
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
                log::info!("AI proxy shutting down");
            })
            .await
            .unwrap_or_else(|e| log::error!("AI proxy error: {e}"));
    });

    let handle = ProxyServerHandle {
        shutdown_tx,
        port: bound_port,
        backend_port,
    };

    // We return a new handle; store a separate one for `stop_proxy()`.
    // Since oneshot::Sender isn't Clone, we store port info only for status checks.
    // The caller owns the real shutdown_tx.

    Ok(handle)
}

/// Store a handle reference for stop/status. Called by the command layer after `start_proxy`.
pub async fn store_handle(handle_port: u16, backend_port: u16) {
    // We can't store the real oneshot sender here (not Clone).
    // The command layer keeps it. We just track running state.
    let mut guard = PROXY_HANDLE.lock().await;
    // Create a dummy sender we'll never use — the real shutdown is via the handle the caller owns.
    let (tx, _rx) = tokio::sync::oneshot::channel();
    *guard = Some(ProxyServerHandle {
        shutdown_tx: tx,
        port: handle_port,
        backend_port,
    });
}

/// Stop the proxy server by dropping the stored handle.
pub fn stop_proxy() {
    // Use try_lock to avoid async — if we can't lock, server is mid-operation and will stop soon.
    if let Ok(mut guard) = PROXY_HANDLE.try_lock() {
        if let Some(handle) = guard.take() {
            let _ = handle.shutdown_tx.send(());
            log::info!("AI proxy stop signal sent");
        }
    }
}

/// Hot-reload model groups without restarting the server.
pub async fn update_model_groups(groups: Vec<ProxyModelGroup>) {
    if let Some(refs) = SHARED_STATE.lock().await.as_ref() {
        refs.model_groups.write().await.update(groups);
        log::info!("AI proxy model groups updated");
    }
}

/// Snapshot current usage statistics.
pub async fn get_usage_stats() -> Option<UsageStats> {
    SHARED_STATE
        .lock()
        .await
        .as_ref()
        .map(|refs| refs.usage.stats())
}

/// Reset all usage counters.
pub async fn reset_usage() {
    if let Some(refs) = SHARED_STATE.lock().await.as_ref() {
        refs.usage.reset();
    }
}

/// Get current proxy status (for Tauri commands).
pub async fn get_status() -> ProxyStatus {
    let guard = PROXY_HANDLE.lock().await;
    match guard.as_ref() {
        Some(h) => ProxyStatus {
            running: true,
            proxy_port: Some(h.port),
            backend_port: Some(h.backend_port),
            backend_installed: true,
        },
        None => ProxyStatus {
            running: false,
            proxy_port: None,
            backend_port: None,
            backend_installed: false,
        },
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// Headers to forward from client to backend.
const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "authorization",
    "anthropic-version",
    "x-api-key",
    "accept",
    "user-agent",
];

/// Copy relevant headers from the incoming request.
fn forward_headers(source: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for key in FORWARDED_HEADERS {
        if let Some(val) = source.get(*key) {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_bytes()) {
                if let Ok(v) = reqwest::header::HeaderValue::from_bytes(val.as_bytes()) {
                    out.insert(name, v);
                }
            }
        }
    }
    out
}

/// The critical completions handler — transform, forward, stream, failover.
async fn handle_completions(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let headers = parts.headers;

    // Read full body
    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Failed to read body: {e}")).into_response();
        }
    };

    let mut body_json: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid JSON: {e}")).into_response();
        }
    };

    // Apply transform pipeline
    let groups = state.model_groups.read().await;
    let transform = transforms::apply_transforms(&mut body_json, &groups);
    drop(groups); // Release read lock

    let is_streaming = body_json
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Track usage
    let provider = infer_provider(&transform.model);
    state.usage.record(&transform.model, &provider);

    // Attempt with failover
    let mut tried_targets: Vec<String> = vec![transform.model.clone()];
    let group_name = transform.group_name.clone();
    let plain_model = transform.plain_model.clone();
    let max_attempts = 3;

    for attempt in 0..max_attempts {
        let fwd_headers = forward_headers(&headers);
        let serialized = serde_json::to_vec(&body_json).unwrap_or_default();

        let backend_url = state.backend_url(&path);
        let result = state
            .client
            .post(&backend_url)
            .headers(fwd_headers)
            .body(serialized)
            .send()
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status();

                // Failover on 429 or 5xx if we have a group to failover within
                if (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
                    && attempt < max_attempts - 1
                {
                    let groups = state.model_groups.read().await;
                    let next = if let Some(ref gn) = group_name {
                        groups.failover(gn, &tried_targets)
                    } else if let Some(ref plain) = plain_model {
                        groups.failover_plain_model(plain, &tried_targets)
                    } else {
                        None
                    };

                    if let Some(next) = next {
                        let next_target = next.target();
                        log::warn!(
                            "Failover: {} ({status}) → {}",
                            tried_targets.last().unwrap_or(&String::new()),
                            next_target
                        );
                        body_json["model"] = json!(next_target.clone());
                        state.usage.record(&next_target, &next.provider);
                        tried_targets.push(next_target);
                        continue;
                    }
                }

                return build_proxy_response(resp, is_streaming).await;
            }
            Err(e) => {
                // Network error — try failover if possible
                if attempt < max_attempts - 1 {
                    let groups = state.model_groups.read().await;
                    let next = if let Some(ref gn) = group_name {
                        groups.failover(gn, &tried_targets)
                    } else if let Some(ref plain) = plain_model {
                        groups.failover_plain_model(plain, &tried_targets)
                    } else {
                        None
                    };

                    if let Some(next) = next {
                        let next_target = next.target();
                        log::warn!("Failover (network error): {e} → {next_target}");
                        body_json["model"] = json!(next_target.clone());
                        state.usage.record(&next_target, &next.provider);
                        tried_targets.push(next_target);
                        continue;
                    }
                }
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("Backend request failed: {e}"),
                )
                    .into_response();
            }
        }
    }

    // Should not reach here, but just in case
    (StatusCode::BAD_GATEWAY, "All failover attempts exhausted").into_response()
}

/// Build an axum Response from a reqwest Response, streaming if needed.
async fn build_proxy_response(resp: reqwest::Response, is_streaming: bool) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = Response::builder().status(status);

    // Forward response headers
    for (key, value) in resp.headers() {
        if let Ok(name) = axum::http::header::HeaderName::from_bytes(key.as_ref()) {
            if let Ok(val) = HeaderValue::from_bytes(value.as_bytes()) {
                builder = builder.header(name, val);
            }
        }
    }

    if is_streaming {
        // Stream SSE response back using bytes_stream
        let stream = resp.bytes_stream();
        builder.body(Body::from_stream(stream)).unwrap_or_else(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Stream error: {e}"),
            )
                .into_response()
        })
    } else {
        // Buffer and forward
        match resp.bytes().await {
            Ok(bytes) => builder.body(Body::from(bytes)).unwrap_or_else(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Body error: {e}"),
                )
                    .into_response()
            }),
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                format!("Failed to read backend response: {e}"),
            )
                .into_response(),
        }
    }
}

/// GET /v1/models — forward to backend, inject model group names.
async fn handle_models(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let fwd_headers = forward_headers(req.headers());
    let url = state.backend_url("/v1/models");

    let resp = match state.client.get(&url).headers(fwd_headers).send().await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("Backend error: {e}")).into_response();
        }
    };

    let status = resp.status();
    let mut body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                format!("Failed to parse models response: {e}"),
            )
                .into_response();
        }
    };

    // Inject group names as synthetic model entries
    let groups = state.model_groups.read().await;
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let synthetic: Vec<Value> = groups
        .group_names()
        .into_iter()
        .map(|name| {
            json!({
                "id": name,
                "object": "model",
                "created": now,
                "owned_by": "jean-proxy",
            })
        })
        .collect();

    if let Some(data) = body.get_mut("data").and_then(Value::as_array_mut) {
        data.extend(synthetic);
    }

    (
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK),
        Json(body),
    )
        .into_response()
}

/// GET /proxy/status — proxy health check.
async fn handle_proxy_status(State(state): State<ProxyState>) -> Json<Value> {
    let groups = state.model_groups.read().await;
    Json(json!({
        "running": true,
        "backend_port": state.backend_port,
        "model_groups": groups.group_names(),
        "uptime_secs": state.usage.uptime().as_secs(),
    }))
}

/// GET /proxy/usage — usage statistics.
async fn handle_proxy_usage(State(state): State<ProxyState>) -> Json<UsageStats> {
    Json(state.usage.stats())
}

/// POST /proxy/usage/reset — reset usage counters.
async fn handle_usage_reset(State(state): State<ProxyState>) -> StatusCode {
    state.usage.reset();
    StatusCode::NO_CONTENT
}

/// Fallback: forward any unmatched request to backend as-is.
async fn handle_passthrough(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let method = req.method().clone();
    let path = req.uri().path_and_query().map_or("/", |pq| pq.as_str());
    let url = state.backend_url(path);
    let headers = forward_headers(req.headers());

    let body_bytes = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Failed to read body: {e}")).into_response();
        }
    };

    let builder = match method {
        Method::GET => state.client.get(&url),
        Method::POST => state.client.post(&url),
        Method::PUT => state.client.put(&url),
        Method::DELETE => state.client.delete(&url),
        Method::PATCH => state.client.patch(&url),
        Method::HEAD => state.client.head(&url),
        _ => state.client.request(method, &url),
    };

    let resp = match builder
        .headers(headers)
        .body(body_bytes.to_vec())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("Backend error: {e}")).into_response();
        }
    };

    build_proxy_response(resp, false).await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Best-effort provider inference from model name.
fn infer_provider(model: &str) -> String {
    if let Some((provider, _)) = model.split_once('/') {
        let provider = provider.trim();
        if !provider.is_empty() {
            return provider.to_string();
        }
    }

    if model.starts_with("claude") {
        "claude".into()
    } else if model.starts_with("gpt") || model.starts_with("o1") || model.starts_with("o3") {
        "openai".into()
    } else if model.starts_with("gemini") {
        "gemini".into()
    } else {
        "unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_provider_known() {
        assert_eq!(infer_provider("claude-sonnet-4"), "claude");
        assert_eq!(infer_provider("gpt-4o"), "openai");
        assert_eq!(infer_provider("o3-mini"), "openai");
        assert_eq!(infer_provider("gemini-2.5-pro"), "gemini");
        assert_eq!(infer_provider("openai/gpt-4o"), "openai");
        assert_eq!(infer_provider("anthropic/claude-sonnet-4"), "anthropic");
        assert_eq!(infer_provider("mistral-large"), "unknown");
    }

    #[test]
    fn forward_headers_filters() {
        let mut hm = HeaderMap::new();
        hm.insert("content-type", HeaderValue::from_static("application/json"));
        hm.insert("authorization", HeaderValue::from_static("Bearer tok"));
        hm.insert("x-custom", HeaderValue::from_static("ignored"));

        let forwarded = forward_headers(&hm);
        assert!(forwarded.get("content-type").is_some());
        assert!(forwarded.get("authorization").is_some());
        assert!(forwarded.get("x-custom").is_none());
    }
}
