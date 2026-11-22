//! Whiteboard relay: serves the page, makes boards, and relays strokes between
//! the browsers on a board. See PROTOCOL.md for the wire contract.

pub mod db;
pub mod keys;
pub mod limits;
pub mod room;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use db::{Db, Role};
use limits::{Bucket, Budget, IpGuard, IpLimits};
use room::{In, Room, MAX_MSG};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::broadcast::error::RecvError, time::Instant};

#[derive(Clone, Debug)]
pub struct Config {
    /// SQLite file.
    pub db_path: String,
    /// Boards not opened for this many days are deleted. 0: keep for ever.
    pub ttl_days: u32,
    /// Take the client address from X-Forwarded-For (set only behind a proxy
    /// you run, which overwrites or appends that header).
    pub trust_proxy: bool,
    /// Serve the page from this directory instead of the copy in the binary.
    pub web_dir: Option<PathBuf>,
}

struct App {
    cfg: Config,
    db: Db,
    rooms: tokio::sync::Mutex<HashMap<i64, Arc<Room>>>,
    ips: IpLimits,
    next_conn: AtomicU64,
    assets: Vec<Asset>,
}

struct Asset {
    route: &'static str,
    file: &'static str,
    mime: &'static str,
    body: &'static [u8],
    etag: String,
}

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn assets() -> Vec<Asset> {
    let raw: [(&str, &str, &str, &[u8]); 6] = [
        ("/", "index.html", "text/html; charset=utf-8", include_bytes!("../../web/index.html")),
        ("/app.js", "app.js", "text/javascript; charset=utf-8", include_bytes!("../../web/app.js")),
        ("/app.css", "app.css", "text/css; charset=utf-8", include_bytes!("../../web/app.css")),
        ("/smooth.wasm", "smooth.wasm", "application/wasm", include_bytes!("../../web/smooth.wasm")),
        ("/crumple.wasm", "crumple.wasm", "application/wasm", include_bytes!("../../web/crumple.wasm")),
        ("/favicon.svg", "favicon.svg", "image/svg+xml", include_bytes!("../../web/favicon.svg")),
    ];
    raw.into_iter()
        .map(|(route, file, mime, body)| Asset { route, file, mime, body, etag: etag(body) })
        .collect()
}

fn etag(body: &[u8]) -> String {
    let d = Sha256::digest(body);
    let hex: String = d[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

fn secure(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow"),
    );
    res
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    secure((status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

async fn serve_asset(app: &App, route: &str, headers: &HeaderMap) -> Response {
    let Some(a) = app.assets.iter().find(|a| a.route == route) else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };
    let (body, tag): (Vec<u8>, String) = match &app.cfg.web_dir {
        Some(dir) => match tokio::fs::read(dir.join(a.file)).await {
            Ok(b) => {
                let t = etag(&b);
                (b, t)
            }
            Err(_) => return plain(StatusCode::NOT_FOUND, "not found"),
        },
        None => (Vec::new(), a.etag.clone()),
    };
    let tag_value = HeaderValue::from_str(&tag).expect("hex etag");
    if headers.get(header::IF_NONE_MATCH) == Some(&tag_value) {
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        res.headers_mut().insert(header::ETAG, tag_value);
        return secure(res);
    }
    let body = if app.cfg.web_dir.is_some() { Body::from(body) } else { Body::from(a.body) };
    let mut res = Response::new(body);
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(a.mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::ETAG, tag_value);
    secure(res)
}

/// A browser page on another site must not be able to use its visitors to
/// make boards or open sockets here. Browsers always send Origin on those
/// requests; other clients may omit it.
fn foreign_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin_host = origin.split_once("://").map_or(origin, |(_, h)| h);
    !origin_host.eq_ignore_ascii_case(host)
}

fn client_ip(app: &App, headers: &HeaderMap, addr: SocketAddr) -> IpAddr {
    if app.cfg.trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        if let Some(ip) = forwarded {
            return ip;
        }
    }
    addr.ip()
}

async fn create_board(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if foreign_origin(&headers) {
        return plain(StatusCode::FORBIDDEN, "wrong origin");
    }
    let ip = client_ip(&app, &headers, addr);
    if !app.ips.may_create(ip) {
        return plain(StatusCode::TOO_MANY_REQUESTS, "too many new boards; wait a moment");
    }
    for _ in 0..3 {
        let edit = keys::random_key();
        let view = keys::view_key(&edit);
        if app.db.create_board(keys::hash(&edit), keys::hash(&view)).await.is_some() {
            let body = format!(r#"{{"edit":"{edit}","view":"{view}"}}"#);
            let mut res = (StatusCode::CREATED, body).into_response();
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return secure(res);
        }
    }
    plain(StatusCode::INTERNAL_SERVER_ERROR, "could not make a board")
}

async fn ws_upgrade(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if foreign_origin(&headers) {
        return plain(StatusCode::FORBIDDEN, "wrong origin");
    }
    let ip = client_ip(&app, &headers, addr);
    let Some(guard) = app.ips.connect(ip) else {
        return plain(StatusCode::TOO_MANY_REQUESTS, "too many connections from this address");
    };
    ws.max_message_size(MAX_MSG)
        .max_frame_size(MAX_MSG)
        .on_upgrade(move |sock| connection(app, sock, guard))
}

fn err_msg(code: &str, m: &str) -> Message {
    Message::Text(format!(r#"{{"t":"err","code":"{code}","m":"{m}"}}"#).into())
}

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

async fn send(sock: &mut WebSocket, m: Message) -> bool {
    matches!(tokio::time::timeout(SEND_TIMEOUT, sock.send(m)).await, Ok(Ok(())))
}

async fn bye(mut sock: WebSocket, code: &str, m: &str) {
    send(&mut sock, err_msg(code, m)).await;
    hang_up(sock).await;
}

/// Close politely: send a close frame, then read until the peer closes too.
/// Dropping a socket with unread data resets it, and the peer can lose the
/// last message we sent.
async fn hang_up(mut sock: WebSocket) {
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        if sock.send(Message::Close(None)).await.is_ok() {
            while let Some(Ok(_)) = sock.recv().await {}
        }
    })
    .await;
}

async fn connection(app: Arc<App>, mut sock: WebSocket, guard: IpGuard) {
    // The first message must be a join.
    let first = tokio::time::timeout(Duration::from_secs(10), sock.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return bye(sock, "bad", "expected a join message").await;
    };
    let Ok(In::Join { key, who }) = serde_json::from_str::<In>(&text) else {
        return bye(sock, "bad", "expected a join message").await;
    };
    if !keys::valid_key(&key) {
        return bye(sock, "gone", "no such board").await;
    }
    let Some((board, role)) = app.db.lookup(keys::hash(&key)).await else {
        return bye(sock, "gone", "no such board").await;
    };
    let who_hash = match (role, who.as_deref()) {
        (Role::Edit, Some(w)) if keys::valid_who(w) => Some(keys::hash(w)),
        (Role::Edit, _) => return bye(sock, "bad", "an edit link needs a contributor secret").await,
        (Role::View, _) => None,
    };
    let view = (role == Role::Edit).then(|| keys::view_key(&key));
    let conn = app.next_conn.fetch_add(1, Ordering::Relaxed);

    // Find or load the board and join it, all under the registry lock, so a
    // board can't be unloaded between the two.
    let (room, joined) = {
        let mut rooms = app.rooms.lock().await;
        let room = match rooms.get(&board) {
            Some(r) => r.clone(),
            None => {
                let r = Arc::new(Room::new(board, app.db.load(board).await));
                rooms.insert(board, r.clone());
                r
            }
        };
        let joined = room.join(conn, role, who_hash, view.as_deref(), &app.db);
        if joined.is_err() && room.conns() == 0 {
            rooms.remove(&board);
        }
        (room, joined)
    };
    let Ok(mut joined) = joined else {
        return bye(sock, "busy", "this board has too many connections").await;
    };

    let reason = relay(&app, &room, &mut sock, &mut joined, Budget::new(guard.out())).await;

    {
        let mut rooms = app.rooms.lock().await;
        room.leave(joined.peer);
        if room.conns() == 0 {
            rooms.remove(&board);
        }
    }
    match reason {
        Some((code, m)) => bye(sock, code, m).await,
        None => hang_up(sock).await,
    }
}

/// Pump messages both ways until the connection ends. Returns an error to
/// report to the client, if any.
async fn relay(
    app: &App,
    room: &Room,
    sock: &mut WebSocket,
    joined: &mut room::Joined,
    mut budget: Budget,
) -> Option<(&'static str, &'static str)> {
    for text in joined.snapshot.drain(..) {
        if !send(sock, Message::Text(text)).await {
            return None;
        }
    }
    let peer = joined.peer;
    let mut bucket = Bucket::new(600.0, 300.0);
    // Bytes: a burst of 2 MB (a few very long strokes), then 256 KB a second.
    let mut bytes = Bucket::new(2.0 * 1024.0 * 1024.0, 256.0 * 1024.0);
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            m = sock.recv() => match m {
                Some(Ok(Message::Text(t))) => {
                    heard = Instant::now();
                    if !bucket.take() || !bytes.take_n(t.len() as f64) {
                        return Some(("rate", "sending too fast"));
                    }
                    room.handle(peer, &t, &app.db, &mut budget);
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => heard = Instant::now(),
                Some(Ok(Message::Binary(_) | Message::Close(_))) | Some(Err(_)) | None => return None,
            },
            out = joined.rx.recv() => match out {
                Ok(out) => {
                    if out.skip == peer.conn || (out.only != 0 && out.only != peer.conn) {
                        continue;
                    }
                    if !send(sock, Message::Text(out.text)).await {
                        return None;
                    }
                }
                // Too far behind: drop the connection; the page reconnects
                // and gets a fresh snapshot.
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => return None,
            },
            _ = ping.tick() => {
                if heard.elapsed() > Duration::from_secs(80) {
                    return None;
                }
                if !send(sock, Message::Ping(Default::default())).await {
                    return None;
                }
            }
        }
    }
}

async fn janitor(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    let mut n = 0u32;
    loop {
        tick.tick().await;
        app.ips.prune();
        if app.cfg.ttl_days > 0 && n % 6 == 0 {
            let skip: Vec<i64> = app.rooms.lock().await.keys().copied().collect();
            let cutoff = db::now() - i64::from(app.cfg.ttl_days) * 86_400;
            let gone = app.db.sweep(cutoff, skip).await;
            if gone > 0 {
                eprintln!("wb-relay: deleted {gone} boards not opened for {} days", app.cfg.ttl_days);
            }
        }
        n = n.wrapping_add(1);
    }
}

/// Run the relay on `listener` until `stop` resolves.
pub async fn serve(
    listener: TcpListener,
    cfg: Config,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let db = Db::open(&cfg.db_path).map_err(std::io::Error::other)?;
    let app = Arc::new(App {
        cfg,
        db: db.clone(),
        rooms: tokio::sync::Mutex::new(HashMap::new()),
        ips: IpLimits::default(),
        next_conn: AtomicU64::new(1),
        assets: assets(),
    });
    let jan = tokio::spawn(janitor(app.clone()));

    async fn page(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
        serve_asset(&app, "/", &headers).await
    }
    async fn file(
        State(app): State<Arc<App>>,
        uri: axum::http::Uri,
        headers: HeaderMap,
    ) -> Response {
        serve_asset(&app, uri.path(), &headers).await
    }
    async fn fallback(method: Method) -> Response {
        if method == Method::GET {
            plain(StatusCode::NOT_FOUND, "not found")
        } else {
            plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
        }
    }

    let router = Router::new()
        .route("/", get(page))
        .route("/b/{key}", get(page))
        .route("/app.js", get(file))
        .route("/app.css", get(file))
        .route("/smooth.wasm", get(file))
        .route("/crumple.wasm", get(file))
        .route("/favicon.svg", get(file))
        .route("/favicon.ico", get(|| async { plain(StatusCode::NO_CONTENT, "") }))
        .route("/api/boards", post(create_board))
        .route("/ws", get(ws_upgrade))
        .route("/healthz", get(|| async { plain(StatusCode::OK, "ok") }))
        .fallback(fallback)
        .with_state(app);

    let res = tokio::select! {
        r = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()) => r,
        _ = stop => Ok(()),
    };
    jan.abort();
    db.flush().await;
    res
}

