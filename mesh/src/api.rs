use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
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

/// Deletes one of this node's own messages, on every node.
async fn delete(State(node): State<Arc<Node>>, Path((room, seq)): Path<(String, u64)>) -> Response {
    match node.delete_own(&room, seq) {
        Ok(_) => Json(json!({ "deleted": seq })).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct Clear {
    /// true: for everyone on the mesh; false: only on this machine.
    #[serde(default)]
    everyone: bool,
}

async fn clear(State(node): State<Arc<Node>>, Path(room): Path<String>, v: Option<Extension<Visitor>>, Json(c): Json<Clear>) -> Response {
    if !c.everyone {
        if let Some(r) = visitor_refused(v, "clearing just this machine's view") {
            return r;
        }
    }
    match node.clear(&room, c.everyone) {
        Ok(()) => Json(json!({ "cleared": room, "everyone": c.everyone })).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

/// Renames this machine; an empty name goes back to the host name.
async fn set_name(State(node): State<Arc<Node>>, v: Option<Extension<Visitor>>, Json(b): Json<NameBody>) -> Response {
    if let Some(r) = visitor_refused(v, "renaming the machine") {
        return r;
    }
    let Some(dir) = node.state_dir.clone() else { return err(anyhow::anyhow!("this node has no state directory")) };
    match crate::daemon::set_name(&dir, &b.name) {
        Ok(()) => {
            node.set_name(crate::daemon::name(&dir));
            Json(json!({ "name": node.name() })).into_response()
        }
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct PassBody {
    passphrase: String,
}

/// Joins the mesh or changes its passphrase. The node then starts again
/// with it; the answer goes out first.
async fn set_passphrase(State(node): State<Arc<Node>>, v: Option<Extension<Visitor>>, Json(b): Json<PassBody>) -> Response {
    if let Some(r) = visitor_refused(v, "changing the passphrase") {
        return r;
    }
    let Some(dir) = node.state_dir.clone() else { return err(anyhow::anyhow!("this node has no state directory")) };
    match crate::daemon::init(&dir, &b.passphrase) {
        Ok(_) => {
            let n = node.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                n.restart.notify_one();
            });
            Json(json!({ "restarting": true })).into_response()
        }
        Err(e) => err(e),
    }
}

async fn rooms(State(node): State<Arc<Node>>) -> Response {
    match node.with_store(|s| s.rooms()) {
        Ok(r) => Json(json!({ "rooms": r })).into_response(),
        Err(e) => err(e),
    }
}

async fn status(State(node): State<Arc<Node>>, v: Option<Extension<Visitor>>) -> Response {
    let entries = node.with_store(|s| s.count()).unwrap_or(0);
    Json(json!({
        "node": node.id_hex(),
        "name": node.name(),
        "version": env!("CARGO_PKG_VERSION"),
        "visitor": v.is_some(),
        "joined": !node.scout,
        "entries": entries,
        "links": node.links(),
        "nearby": node.nearby(),
    }))
    .into_response()
}

/// Marks a request that came through this machine's web server from another
/// device, signed in with the mesh passphrase. Such a visitor may read, post
/// and clear for everyone, but not change this machine's own settings.
#[derive(Clone, Copy)]
struct Visitor;

const SESSION_COOKIE: &str = "elemesh_session";

fn sessions() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// How long an invite link works.
const INVITE_FOR: Duration = Duration::from_secs(24 * 3600);
/// How long a visiting device stays signed in.
const SESSION_MAX_AGE_S: u64 = 30 * 24 * 3600;

/// Invite tokens and when each stops working. One device each.
fn invites() -> &'static Mutex<HashMap<String, std::time::Instant>> {
    static I: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    I.get_or_init(Default::default)
}

fn session_of(req: &Request) -> Option<String> {
    let cookies = req.headers().get(header::COOKIE)?.to_str().ok()?;
    cookies
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_string())
}

fn peer_is_loopback(req: &Request) -> bool {
    req.extensions().get::<ConnectInfo<SocketAddr>>().is_some_and(|c| c.0.ip().is_loopback())
}

/// A program or browser on this machine talking to the API directly: not
/// through a proxy, and addressed to localhost, so a web page elsewhere can't
/// reach it through DNS rebinding.
fn direct_local(req: &Request) -> bool {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let proxied = req.headers().contains_key("x-forwarded-for") || req.headers().contains_key("x-real-ip");
    peer_is_loopback(req) && !proxied && matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

/// Who may use the API. This machine itself: everything. A device reaching
/// the mesh page through this machine's web server: the page and the sign-in
/// always, the rest once signed in with the mesh passphrase.
async fn gate(mut req: Request, next: Next) -> Response {
    if direct_local(&req) {
        return next.run(req).await;
    }
    // The API only listens on loopback, so anything else came through a proxy
    // on this machine (nginx sharing the page as /elem/).
    if !peer_is_loopback(&req) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = req.uri().path();
    if path == "/" || path == "/v1/login" {
        return next.run(req).await;
    }
    let signed_in = session_of(&req).is_some_and(|t| sessions().lock().unwrap().contains(&t));
    if !signed_in {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "enter the mesh passphrase", "login": true }))).into_response();
    }
    req.extensions_mut().insert(Visitor);
    next.run(req).await
}

#[derive(Deserialize)]
struct Login {
    #[serde(default)]
    passphrase: String,
    #[serde(default)]
    invite: String,
}

/// Signs a visiting device in, with the mesh passphrase or an invite.
/// Deriving the key is deliberately slow, and a wrong guess waits a second
/// more. An invite works once, until it expires.
async fn login(State(node): State<Arc<Node>>, Json(l): Json<Login>) -> Response {
    let ok = if node.scout {
        false
    } else if !l.invite.is_empty() {
        let mut inv = invites().lock().unwrap();
        inv.retain(|_, until| *until > std::time::Instant::now());
        inv.remove(&l.invite).is_some()
    } else {
        let psk = crate::identity::derive_psk(&l.passphrase).ok();
        psk.is_some_and(|k| k.iter().zip(node.psk.iter()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0)
    };
    if !ok {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let why = if l.invite.is_empty() { "that isn't this mesh's passphrase" } else { "this invite has been used or has expired" };
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": why }))).into_response();
    }
    let token = hex::encode(rand::random::<[u8; 16]>());
    sessions().lock().unwrap().insert(token.clone());
    let cookie = format!("{SESSION_COOKIE}={token}; Path=/; Max-Age={SESSION_MAX_AGE_S}; HttpOnly; SameSite=Lax");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "signed_in": true }))).into_response()
}

/// Makes an invite: a token that lets one more device in for a day, so a
/// link can be shared without sharing the passphrase.
async fn invite(State(node): State<Arc<Node>>) -> Response {
    if node.scout {
        return err(anyhow::anyhow!("join the mesh before inviting anyone"));
    }
    let token = hex::encode(rand::random::<[u8; 16]>());
    invites().lock().unwrap().insert(token.clone(), std::time::Instant::now() + INVITE_FOR);
    Json(json!({ "invite": token, "expires_s": INVITE_FOR.as_secs() })).into_response()
}

fn visitor_refused(v: Option<Extension<Visitor>>, what: &str) -> Option<Response> {
    v.map(|_| (StatusCode::FORBIDDEN, Json(json!({ "error": format!("{what} only on this machine itself") }))).into_response())
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
        .route("/v1/rooms/{room}/messages/{seq}", axum::routing::delete(delete))
        .route("/v1/rooms/{room}/clear", axum::routing::post(clear))
        .route("/v1/name", axum::routing::put(set_name))
        .route("/v1/passphrase", axum::routing::post(set_passphrase))
        .route("/v1/login", axum::routing::post(login))
        .route("/v1/invite", axum::routing::post(invite))
        .layer(middleware::from_fn(gate))
        .with_state(node);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("api: http://{addr}/");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
