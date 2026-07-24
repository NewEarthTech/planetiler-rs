use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Json, Response};
use axum::routing::get;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;

use crate::auth::Auth;
use crate::event::LogEvent;
use crate::hub::{RunHub, Subscription};

/// Bounded handoff between the per-client pump task and the SSE body; a
/// client that stops reading eventually blocks its own pump, never the hub.
const CLIENT_QUEUE: usize = 256;

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<RunHub>,
    pub auth: Arc<Auth>,
    /// Origins allowed to call us from a browser — the console is a
    /// different origin (runner. vs live.runner.), so every SPA request is
    /// CORS, and the Authorization header makes it preflighted.
    pub allowed_origins: Vec<HeaderValue>,
}

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(state.allowed_origins.clone())
        .allow_methods([Method::GET])
        .allow_headers([
            header::AUTHORIZATION,
            HeaderName::from_static("last-event-id"),
        ])
        .max_age(Duration::from_secs(3600));
    let authed = Router::new()
        .route("/runs", get(list_runs))
        .route("/runs/{run}/events", get(events))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));
    Router::new()
        .route("/healthz", get(async || "ok"))
        .merge(authed)
        // Outermost so preflights are answered before auth sees them.
        .layer(cors)
        .with_state(state)
}

async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Auth::Cognito(auth) = &*state.auth else {
        return Ok(next.run(request).await);
    };
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    auth.verify(token).await.map_err(|err| {
        tracing::debug!(error = %format!("{err:#}"), "rejected token");
        StatusCode::UNAUTHORIZED
    })?;
    Ok(next.run(request).await)
}

async fn list_runs(State(state): State<AppState>) -> Json<Vec<crate::hub::RunSummary>> {
    Json(state.hub.runs())
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Resume point for clients that can't set headers; `Last-Event-ID` wins.
    after: Option<u64>,
}

/// The client leg: ring-buffer history first, then live, one response —
/// Vercel `?follow=1` semantics. Event ids are the per-run seq, so a
/// reconnect with `Last-Event-ID` resumes exactly where the stream broke.
async fn events(
    State(state): State<AppState>,
    Path(run): Path<String>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .or(query.after)
        .unwrap_or(0);
    let subscription = state.hub.subscribe(&run, after);
    let (tx, rx) = mpsc::channel::<Arc<LogEvent>>(CLIENT_QUEUE);
    tokio::spawn(pump(state.hub.clone(), run, subscription, after, tx));
    let stream = ReceiverStream::new(rx).map(|event| {
        Ok(Event::default()
            .id(event.seq.to_string())
            .event("line")
            .json_data(&*event)
            .unwrap_or_else(|_| Event::default().comment("serialize error")))
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Splices backfill and live for one client. The `attachment` field of the
/// subscription rides along so the hub's watcher count drops the moment this
/// task ends (client gone → send fails → return).
async fn pump(
    hub: Arc<RunHub>,
    run: String,
    subscription: Subscription,
    after: u64,
    tx: mpsc::Sender<Arc<LogEvent>>,
) {
    let Subscription {
        backfill,
        mut live,
        attachment: _attachment,
    } = subscription;
    let mut last = after;
    for event in backfill {
        last = event.seq;
        if tx.send(event).await.is_err() {
            return;
        }
    }
    loop {
        match live.recv().await {
            Ok(event) => {
                // Already delivered from the buffer snapshot.
                if event.seq <= last {
                    continue;
                }
                last = event.seq;
                if tx.send(event).await.is_err() {
                    return;
                }
            }
            // Lagged behind the broadcast: heal the gap from the ring buffer,
            // which holds far more than the channel does.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                for event in hub.since(&run, last) {
                    last = event.seq;
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::StreamKind;
    use tokio::io::AsyncReadExt;

    async fn serve(state: AppState) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        addr
    }

    fn state_with_lines(n: u64) -> AppState {
        let hub = RunHub::new(10_000);
        for i in 1..=n {
            hub.publish("r1", StreamKind::Planetiler, i as i64, format!("line {i}"));
        }
        AppState {
            hub,
            auth: Arc::new(Auth::Disabled),
            allowed_origins: vec![HeaderValue::from_static("https://console.test")],
        }
    }

    /// Read from the raw socket until `needle` shows up (SSE bodies never
    /// end, so a plain body read would hang).
    async fn get_until(
        addr: std::net::SocketAddr,
        path: &str,
        extra: &str,
        needle: &str,
    ) -> String {
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut conn,
            format!("GET {path} HTTP/1.1\r\nHost: test\r\n{extra}Connection: keep-alive\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut collected = String::new();
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !collected.contains(needle) {
            let read = tokio::time::timeout_at(deadline, conn.read(&mut buf))
                .await
                .expect("timed out waiting for SSE data")
                .unwrap();
            assert!(read > 0, "connection closed before needle");
            collected.push_str(&String::from_utf8_lossy(&buf[..read]));
        }
        collected
    }

    #[tokio::test]
    async fn events_streams_history_then_live() {
        let state = state_with_lines(3);
        let hub = state.hub.clone();
        let addr = serve(state).await;
        let hub2 = hub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            hub2.publish("r1", StreamKind::Planetiler, 4, "line 4".into());
        });
        let body = get_until(addr, "/runs/r1/events", "", "line 4").await;
        for i in 1..=4 {
            assert!(
                body.contains(&format!("line {i}")),
                "missing line {i}: {body}"
            );
        }
        let first_id = body.find("id: 1").expect("seq ids present");
        let live_line = body.find("line 4").unwrap();
        assert!(first_id < live_line, "history precedes live");
    }

    #[tokio::test]
    async fn last_event_id_resumes_after_delivered_events() {
        let state = state_with_lines(5);
        let addr = serve(state).await;
        let body = get_until(addr, "/runs/r1/events", "Last-Event-ID: 3\r\n", "line 5").await;
        assert!(
            !body.contains("line 3"),
            "resumed stream repeated old lines: {body}"
        );
        assert!(body.contains("line 4") && body.contains("line 5"));
    }

    #[tokio::test]
    async fn cors_preflight_admits_the_console_origin_without_auth() {
        let state = state_with_lines(1);
        let addr = serve(state).await;
        let client = reqwest::Client::new();
        let res = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/runs/r1/events"),
            )
            .header("origin", "https://console.test")
            .header("access-control-request-method", "GET")
            .header(
                "access-control-request-headers",
                "authorization,last-event-id",
            )
            .send()
            .await
            .unwrap();
        assert!(
            res.status().is_success(),
            "preflight failed: {}",
            res.status()
        );
        let allow_origin = res
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(allow_origin, "https://console.test");
        let allow_headers = res
            .headers()
            .get("access-control-allow-headers")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_lowercase();
        assert!(allow_headers.contains("authorization") && allow_headers.contains("last-event-id"));
        // An origin we don't trust gets no allow-origin back.
        let other = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{addr}/runs/r1/events"),
            )
            .header("origin", "https://evil.test")
            .header("access-control-request-method", "GET")
            .send()
            .await
            .unwrap();
        assert!(other.headers().get("access-control-allow-origin").is_none());
    }

    #[tokio::test]
    async fn healthz_is_open_and_runs_lists_activity() {
        let state = state_with_lines(2);
        let addr = serve(state).await;
        let health = reqwest::get(format!("http://{addr}/healthz"))
            .await
            .unwrap();
        assert!(health.status().is_success());
        let runs: Vec<serde_json::Value> = reqwest::get(format!("http://{addr}/runs"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(runs[0]["run"], "r1");
        assert_eq!(runs[0]["lines"], 2);
    }
}
