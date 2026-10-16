use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::node::Node;

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    after: i64,
    #[serde(default)]
    wait: u64,
}

#[derive(Deserialize)]
struct Post {
    #[serde(default)]
    nick: String,
    text: String,
    /// When the message was actually said, if the caller knows better than we
    /// do. Omitted, we stamp it ourselves, as before.
    ///
    /// This exists because a mesh that re-stamps is a mesh that destroys
    /// identity. The suite addresses a message by the hash of its content and
    /// its timestamp, so two nodes holding the same message agree on that with
    /// no coordinator -- but only if the timestamp survives the trip. It did
    /// not: the same message arriving over the LAN and over Bluetooth landed
    /// as two different messages, and the house saw everything twice.
    #[serde(default)]
    ts_ms: Option<u64>,
}

fn err(e: anyhow::Error) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response()
}

async fn messages(State(node): State<Arc<Node>>, Path(room): Path<String>, Query(q): Query<Since>) -> Response {
    let fetch = || node.with_store(|s| s.messages(&room, q.after, 500));
    // Register for wake-ups before reading, so a message landing in between isn't missed.
    let notified = node.changed.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let mut rows = match fetch() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    if rows.is_empty() && q.wait > 0 {
        let _ = tokio::time::timeout(Duration::from_secs(q.wait.min(30)), notified).await;
        rows = fetch().unwrap_or_default();
    }
    let cursor = rows.last().map(|r| r.cursor).unwrap_or(q.after);
    Json(json!({ "cursor": cursor, "messages": rows })).into_response()
}

async fn post(State(node): State<Arc<Node>>, Path(room): Path<String>, Json(p): Json<Post>) -> Response {
    match node.post(&room, p.nick.trim(), &p.text, p.ts_ms) {
        Ok(e) => Json(json!({ "author": hex::encode(e.author), "seq": e.seq, "ts_ms": e.ts_ms })).into_response(),
        Err(e) => err(e),
    }
}

async fn rooms(State(node): State<Arc<Node>>) -> Response {
    match node.with_store(|s| s.rooms()) {
        Ok(r) => Json(json!({ "rooms": r })).into_response(),
        Err(e) => err(e),
    }
}

async fn status(State(node): State<Arc<Node>>) -> Response {
    let entries = node.with_store(|s| s.count()).unwrap_or(0);
    Json(json!({
        "node": node.id_hex(),
        "joined": !node.scout,
        "entries": entries,
        "links": node.links(),
        "nearby": node.nearby(),
    }))
    .into_response()
}

/// Only answer requests addressed to localhost, so a web page cannot reach the
/// daemon through DNS rebinding.
async fn local_only(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        next.run(req).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

pub async fn serve(node: Arc<Node>, addr: SocketAddr) -> anyhow::Result<()> {
    if !addr.ip().is_loopback() {
        anyhow::bail!("refusing to bind the local API to {addr}; use a loopback address");
    }
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("page.html")) }))
        .route("/v1/status", get(status))
        .route("/v1/rooms", get(rooms))
        .route("/v1/rooms/{room}/messages", get(messages).post(post))
        .layer(middleware::from_fn(local_only))
        .with_state(node);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("api: http://{addr}/");
    axum::serve(listener, app).await?;
    Ok(())
}
