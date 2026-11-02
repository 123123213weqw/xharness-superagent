//! Opt-in loopback management transport. Every operation enters the existing
//! SQL owner; HTTP tasks never open a database or accept a model-supplied origin.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::client::Client;
use crate::{Error, Result};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::any,
    Router,
};
use futures::FutureExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{net::Ipv4Addr, time::Duration};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const MAX_BODY: usize = 128 * 1024;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub port: u16,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 {
            return Err(problem(400, "management.port must be 1–65535"));
        }
        Ok(())
    }
}
pub(crate) fn problem(status: u16, message: impl Into<String>) -> Error {
    Error::Management {
        status,
        message: message.into(),
    }
}
pub(super) fn decode_segment(raw: &str) -> Result<String> {
    let mut result = Vec::new();
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let digit = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            let high = bytes
                .next()
                .and_then(digit)
                .ok_or_else(|| problem(400, "Malformed path escape"))?;
            let low = bytes
                .next()
                .and_then(digit)
                .ok_or_else(|| problem(400, "Malformed path escape"))?;
            result.push(high * 16 + low);
        } else {
            result.push(byte);
        }
    }
    let result =
        String::from_utf8(result).map_err(|_| problem(400, "Path identifier is not UTF-8"))?;
    if result.is_empty() || result.chars().any(char::is_control) {
        return Err(problem(400, "Invalid path identifier"));
    }
    Ok(result)
}
#[derive(Clone)]
struct Host {
    client: Client,
    port: u16,
    max_prompt_bytes: usize,
    stop: CancellationToken,
}
fn same_origin(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host_ok = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let origin_ok = headers
        .get("origin")
        .is_none_or(|v| v.to_str().is_ok_and(|v| v == format!("http://{host}")));
    host_ok
        && origin_ok
        && headers
            .get("x-orchestrator-request")
            .is_some_and(|v| v == "1")
}
fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
        ],
        axum::Json(body),
    )
        .into_response()
}
fn failure(error: Error) -> Response {
    use xharness_superagent_core::engine::DomainError;
    let user = match &error {
        Error::PromptUser(user) | Error::Domain(DomainError::User(user)) => Some(user),
        _ => None,
    };
    let status = match &error {
        Error::Management { status, .. } => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Error::SettingsConflict { .. } | Error::PromptConflict { .. } => StatusCode::CONFLICT,
        Error::Json(_) | Error::PromptValidation(_) => StatusCode::BAD_REQUEST,
        _ if user.is_some() => {
            if user.is_some_and(|u| u.missing) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            }
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut body = json!({"error":error.to_string()});
    if let Some(user) = user {
        body["code"] = json!(user.code);
        body["values"] = json!(user.values);
    }
    json_response(status, body)
}
async fn handle(State(host): State<Host>, request: Request<Body>) -> Response {
    let path = request.uri().path();
    if path == "/orchestrator" || path == "/orchestrator/" {
        if request.method() != "GET" {
            return failure(problem(405, "Method not allowed"));
        }
        // This document has no privileged data. Its fetches still need the API
        // guard, and untrusted task text is rendered with textContent only.
        return ([("cache-control", "no-store"),
            ("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
            ("x-content-type-options", "nosniff")], Html(include_str!("management.html"))).into_response();
    }
    let Some(path) = path.strip_prefix("/orchestrator/api/") else {
        return failure(problem(404, "Unknown operation"));
    };
    if !same_origin(request.headers(), host.port) {
        return failure(problem(403, "Same-origin management request required"));
    }
    if path.len() > 2048 || request.uri().query().is_some_and(|q| q.len() > 4096) {
        return failure(problem(400, "Request target too long"));
    }
    if host.stop.is_cancelled() {
        return failure(problem(503, "Management is stopping"));
    }
    let body_limit = if matches!(path, "prompts" | "prompts/preview") {
        host.max_prompt_bytes
    } else {
        MAX_BODY
    };
    let operation = format!("{} {}", request.method(), path);
    let query = request.uri().query().unwrap_or("").to_owned();
    let body = tokio::select! {
        biased;
        _ = host.stop.cancelled() => return failure(problem(503, "Management is stopping")),
        body = tokio::time::timeout(Duration::from_secs(5), to_bytes(request.into_body(), body_limit)) => match body {
            Ok(Ok(body)) => match String::from_utf8(body.to_vec()) { Ok(body) => body, Err(_) => return failure(problem(400, "Request body must be UTF-8")) },
            Ok(Err(_)) => return failure(problem(400, "Request body exceeds the limit or could not be read")),
            Err(_) => return failure(problem(408, "Request body timed out")),
        }
    };
    let cancel = host.stop.child_token();
    let outcome = host
        .client
        .call_with_cancel(cancel.clone(), move |app| {
            let port = app.state.panel.get().cloned();
            async move {
                port.ok_or_else(|| problem(503, "Management owner unavailable"))?(
                    operation,
                    serde_json::to_string(&json!({"body":body,"query":query}))?,
                    cancel,
                )
                .await
            }
            .boxed_local()
        })
        .await;
    match outcome {
        Ok(body) => (
            StatusCode::OK,
            [
                ("content-type", "application/json; charset=utf-8"),
                ("cache-control", "no-store"),
                ("x-content-type-options", "nosniff"),
            ],
            body,
        )
            .into_response(),
        Err(error) => failure(error),
    }
}
/// No listener is created for --check/--once. Dropping/aborting a transport
/// does not roll back an admitted SQL command; Client records a lost response.
pub(super) struct Server {
    listener: Option<TcpListener>,
    router: Router,
    stop: CancellationToken,
}
pub(super) async fn bind(
    config: Option<Config>,
    client: Client,
    stop: CancellationToken,
    prompt_max_chars: usize,
) -> Result<Server> {
    let Some(config) = config else {
        return Ok(Server {
            listener: None,
            router: Router::new(),
            stop,
        });
    };
    config.validate()?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).await?;
    let host = Host {
        client,
        port: config.port,
        max_prompt_bytes: prompt_max_chars.saturating_mul(4).saturating_add(4096),
        stop: stop.clone(),
    };
    Ok(Server {
        listener: Some(listener),
        router: Router::new().fallback(any(handle)).with_state(host),
        stop,
    })
}
impl Server {
    pub(super) async fn run(self) -> Result<()> {
        let Self {
            listener,
            router,
            stop,
        } = self;
        let Some(listener) = listener else {
            stop.cancelled().await;
            return Ok(());
        };
        let ending = stop.clone();
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            ending.cancelled().await;
        });
        let server = std::future::IntoFuture::into_future(server);
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => result.map_err(Error::from),
            _ = stop.cancelled() => match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                Ok(result) => result.map_err(Error::from),
                Err(_) => Err(problem(503, "Management transport did not drain before shutdown deadline")),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn origin_requires_exact_host_marker_and_optional_matching_origin() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:3092".parse().unwrap());
        assert!(!same_origin(&headers, 3092));
        headers.insert("x-orchestrator-request", "1".parse().unwrap());
        assert!(same_origin(&headers, 3092));
        headers.insert("origin", "http://attacker.invalid".parse().unwrap());
        assert!(!same_origin(&headers, 3092));
        headers.insert("origin", "http://127.0.0.1:3092".parse().unwrap());
        assert!(same_origin(&headers, 3092));
        headers.insert("host", "attacker.invalid:3092".parse().unwrap());
        assert!(!same_origin(&headers, 3092));
        assert!(!same_origin(&headers, 3093));
    }
    #[test]
    fn path_decode_preserves_literal_plus_and_rejects_malformed_utf8() {
        assert_eq!(decode_segment("source%3Agithub").unwrap(), "source:github");
        assert_eq!(decode_segment("a+b%2Fc").unwrap(), "a+b/c");
        assert_eq!(decode_segment("%E4%B8%AD%F0%9F%98%80").unwrap(), "中😀");
        for raw in ["", "%", "%xy", "%FF", "%00", "%0a"] {
            assert!(decode_segment(raw).is_err(), "{raw}");
        }
    }
}
