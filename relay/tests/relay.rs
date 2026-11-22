//! End-to-end tests: a real relay on a local port, real WebSocket clients.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use wb_relay::{serve, Config};

struct Srv {
    addr: String,
    db: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

fn temp_db(name: &str) -> String {
    let mut r = [0u8; 6];
    getrandom::fill(&mut r).unwrap();
    let hex: String = r.iter().map(|b| format!("{b:02x}")).collect();
    std::env::temp_dir().join(format!("wb-test-{name}-{hex}.db")).to_string_lossy().into_owned()
}

async fn start(db: &str) -> Srv {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let cfg = Config { db_path: db.to_string(), ttl_days: 0, trust_proxy: false, web_dir: None };
    let task = tokio::spawn(async move {
        serve(listener, cfg, async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    Srv { addr, db: db.to_string(), stop: Some(stop), task }
}

impl Srv {
    async fn stop(mut self) -> String {
        let _ = self.stop.take().unwrap().send(());
        let _ = (&mut self.task).await;
        self.db.clone()
    }
}

async fn http(addr: &str, method: &str, path: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_lowercase(), body.to_string())
}

async fn new_board(addr: &str) -> (String, String) {
    let (status, _, body) = http(addr, "POST", "/api/boards").await;
    assert_eq!(status, 201, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    (v["edit"].as_str().unwrap().into(), v["view"].as_str().unwrap().into())
}

struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

const WHO_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaa";
const WHO_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbb";

impl Client {
    async fn open(addr: &str) -> Client {
        let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        Client { ws }
    }

    /// Join and read up to `ready`. Returns the client, hello and the snapshot strokes.
    async fn join(addr: &str, key: &str, who: Option<&str>) -> (Client, Value, Vec<Value>) {
        let mut c = Client::open(addr).await;
        c.send(json!({"t": "join", "key": key, "who": who})).await;
        let hello = c.next().await;
        assert_eq!(hello["t"], "hello", "{hello}");
        let mut strokes = Vec::new();
        loop {
            let m = c.next().await;
            match m["t"].as_str().unwrap() {
                "ss" => strokes.extend(m["a"].as_array().unwrap().iter().cloned()),
                "ready" => break,
                _ => {}
            }
        }
        (c, hello, strokes)
    }

    async fn send(&mut self, v: Value) {
        self.ws.send(Message::Text(v.to_string().into())).await.unwrap();
    }

    async fn raw(&mut self) -> Option<Value> {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(3), self.ws.next()).await.ok()??;
            match m {
                Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
                Ok(Message::Close(_)) | Err(_) => return None,
                _ => {}
            }
        }
    }

    /// The next message that is not a presence count.
    async fn next(&mut self) -> Value {
        loop {
            let m = self.raw().await.expect("expected a message");
            if m["t"] != "n" {
                return m;
            }
        }
    }

    async fn expect(&mut self, t: &str) -> Value {
        let m = self.next().await;
        assert_eq!(m["t"], t, "{m}");
        m
    }

    /// Nothing but presence counts arrives for a short while.
    async fn quiet(&mut self) {
        loop {
            match tokio::time::timeout(Duration::from_millis(250), self.ws.next()).await {
                Err(_) => return,
                Ok(Some(Ok(Message::Text(t)))) => {
                    let m: Value = serde_json::from_str(&t).unwrap();
                    assert_eq!(m["t"], "n", "unexpected message {m}");
                }
                Ok(_) => return,
            }
        }
    }

    async fn draw(&mut self, id: &str) -> u64 {
        self.send(json!({"t": "s", "id": id, "c": "#112233", "w": 0.004, "pts": [0.1, 0.1, 0.5, 0.2, 0.2, 0.5]})).await;
        self.expect("ok").await["q"].as_u64().unwrap()
    }
}

#[tokio::test]
async fn page_and_headers() {
    let s = start(&temp_db("page")).await;
    let (status, head, body) = http(&s.addr, "GET", "/").await;
    assert_eq!(status, 200);
    assert!(body.contains("<canvas"));
    assert!(head.contains("content-security-policy: default-src 'self'"));
    assert!(head.contains("referrer-policy: no-referrer"));
    assert_eq!(http(&s.addr, "GET", "/b/anything").await.0, 200);
    let (status, head, _) = http(&s.addr, "GET", "/smooth.wasm").await;
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/wasm"));
    assert_eq!(http(&s.addr, "GET", "/app.js").await.0, 200);
    assert_eq!(http(&s.addr, "GET", "/app.css").await.0, 200);
    assert_eq!(http(&s.addr, "GET", "/nope").await.0, 404);
    assert_eq!(http(&s.addr, "GET", "/api/boards").await.0, 405);
    assert_eq!(http(&s.addr, "GET", "/healthz").await.0, 200);
    s.stop().await;
}

#[tokio::test]
async fn board_creation_is_rate_limited() {
    let s = start(&temp_db("rate")).await;
    for _ in 0..10 {
        new_board(&s.addr).await;
    }
    assert_eq!(http(&s.addr, "POST", "/api/boards").await.0, 429);
    s.stop().await;
}

#[tokio::test]
async fn draw_view_and_late_join() {
    let s = start(&temp_db("draw")).await;
    let (edit, view) = new_board(&s.addr).await;
    assert_ne!(edit, view);

    let (mut a, hello, strokes) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    assert_eq!(hello["role"], "edit");
    assert_eq!(hello["me"], 1);
    assert_eq!(hello["view"], view.as_str());
    assert_eq!(hello["layers"][0]["l"], 1);
    assert!(strokes.is_empty());

    let (mut v, vhello, _) = Client::join(&s.addr, &view, None).await;
    assert_eq!(vhello["role"], "view");
    assert!(vhello["me"].is_null());
    assert!(vhello["view"].is_null(), "a viewer must not learn any key");

    a.send(json!({"t": "b", "id": "s1", "c": "#112233", "w": 0.004})).await;
    a.send(json!({"t": "p", "id": "s1", "pts": [0.1, 0.1, 0.5, 0.2, 0.2, 0.6]})).await;
    let b = v.expect("b").await;
    assert_eq!(b["l"], 1);
    assert_eq!(b["c"], "#112233");
    assert_eq!(v.expect("p").await["pts"], json!([0.1, 0.1, 0.5, 0.2, 0.2, 0.6]));

    // Someone joining mid-stroke sees the stroke in progress.
    let (_mid, _, _) = {
        let mut c = Client::open(&s.addr).await;
        c.send(json!({"t": "join", "key": view, "who": null})).await;
        c.expect("hello").await;
        assert_eq!(c.expect("b").await["id"], "s1");
        assert_eq!(c.expect("p").await["id"], "s1");
        c.expect("ready").await;
        (c, (), ())
    };

    a.send(json!({"t": "e", "id": "s1", "c": "#112233", "w": 0.004, "pts": [0.1, 0.1, 0.5, 0.2, 0.2, 0.6]})).await;
    let ok = a.expect("ok").await;
    assert_eq!(ok["q"], 1);
    let e = v.expect("e").await;
    assert_eq!((e["id"].as_str(), e["l"].as_u64(), e["q"].as_u64()), (Some("s1"), Some(1), Some(1)));

    // A viewer can't draw, undo, or delete a layer.
    v.send(json!({"t": "s", "id": "evil", "c": "#000000", "w": 0.004, "pts": [0.1, 0.1, 0.5]})).await;
    v.send(json!({"t": "u", "id": "s1"})).await;
    v.send(json!({"t": "ld", "l": 1})).await;
    a.quiet().await;
    v.quiet().await;

    let (_late, _, strokes) = Client::join(&s.addr, &view, None).await;
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0]["id"], "s1");
    assert_eq!(strokes[0]["pts"], json!([0.1, 0.1, 0.5, 0.2, 0.2, 0.6]));
    s.stop().await;
}

#[tokio::test]
async fn boards_survive_a_restart() {
    let s = start(&temp_db("restart")).await;
    let (edit, view) = new_board(&s.addr).await;
    {
        let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
        let (mut b, hb, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
        assert_eq!(hb["me"], 2);
        a.expect("l").await;
        a.draw("a1").await;
        b.expect("e").await;
        b.draw("b1").await;
        a.expect("e").await;
        a.draw("a2").await;
        a.send(json!({"t": "u", "id": "a2"})).await;
        a.send(json!({"t": "ld", "l": 2})).await;
        a.expect("ld").await;
    }
    let db = s.stop().await;

    let s = start(&db).await;
    let (mut b, hello, strokes) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    assert_eq!(hello["me"], 2, "the same browser keeps its layer");
    assert_eq!(hello["layers"].as_array().unwrap().len(), 2);
    assert_eq!(hello["layers"][0]["del"], false);
    assert_eq!(hello["layers"][1]["del"], true);
    assert_ne!(hello["layers"][0]["c"], hello["layers"][1]["c"]);
    let ids: Vec<&str> = strokes.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a1"], "undone and deleted strokes are not drawn");

    // The deleted layer can still be restored after the restart.
    b.send(json!({"t": "lr", "l": 2})).await;
    let lr = b.expect("lr").await;
    assert_eq!(lr["a"][0]["id"], "b1");
    assert_eq!(lr["a"][0]["q"], 2);
    // Sequence numbers carry on after the highest one ever used.
    assert_eq!(b.draw("b2").await, 4);
    let (_v, _, strokes) = Client::join(&s.addr, &view, None).await;
    assert_eq!(strokes.len(), 3);
    s.stop().await;
}

#[tokio::test]
async fn layer_delete_and_restore() {
    let s = start(&temp_db("layers")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut b, _, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    let l = a.expect("l").await;
    assert_eq!(l["l"], 2);
    a.draw("a1").await;
    a.draw("a2").await;
    b.expect("e").await;
    b.expect("e").await;
    b.draw("b1").await;
    a.expect("e").await;

    // Any editor can delete another contributor's layer; everyone hears it.
    b.send(json!({"t": "ld", "l": 1})).await;
    assert_eq!(a.expect("ld").await["l"], 1);
    assert_eq!(b.expect("ld").await["l"], 1);
    let (_v, hello, strokes) = Client::join(&s.addr, &view, None).await;
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0]["id"], "b1");
    assert_eq!(hello["layers"][0]["del"], true);

    // Strokes drawn after the delete are not part of it.
    a.draw("a3").await;
    b.expect("e").await;

    // Undo: the deleted strokes come back, in order.
    a.send(json!({"t": "lr", "l": 1})).await;
    let lr = b.expect("lr").await;
    a.expect("lr").await;
    let ids: Vec<&str> = lr["a"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a1", "a2"]);
    let (_v2, hello, strokes) = Client::join(&s.addr, &view, None).await;
    assert_eq!(strokes.len(), 4);
    assert_eq!(hello["layers"][0]["del"], false);

    // Deleting an empty layer or restoring a layer with nothing deleted does nothing.
    a.send(json!({"t": "lr", "l": 1})).await;
    a.send(json!({"t": "ld", "l": 77})).await;
    a.quiet().await;
    s.stop().await;
}

#[tokio::test]
async fn undo_is_for_your_own_strokes() {
    let s = start(&temp_db("undo")).await;
    let (edit, _) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut b, _, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    a.expect("l").await;
    a.draw("a1").await;
    b.expect("e").await;

    b.send(json!({"t": "u", "id": "a1"})).await;
    a.quiet().await;

    a.send(json!({"t": "u", "id": "a1"})).await;
    assert_eq!(b.expect("u").await["id"], "a1");

    // Resending an undone stroke does not bring it back.
    a.send(json!({"t": "s", "id": "a1", "c": "#112233", "w": 0.004, "pts": [0.1, 0.1, 0.5]})).await;
    assert_eq!(a.expect("u").await["id"], "a1");
    // And nobody else can take the id.
    b.send(json!({"t": "s", "id": "a1", "c": "#112233", "w": 0.004, "pts": [0.1, 0.1, 0.5]})).await;
    assert_eq!(b.expect("u").await["id"], "a1");
    let (_c, _, strokes) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    assert!(strokes.is_empty());
    s.stop().await;
}

#[tokio::test]
async fn resends_are_stored_once() {
    let s = start(&temp_db("resend")).await;
    let (edit, _) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut b, _, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    a.expect("l").await;
    let q = a.draw("x1").await;
    assert_eq!(a.draw("x1").await, q);
    b.expect("e").await;
    b.quiet().await;

    // An end with no begin (the stroke started offline) is stored whole.
    a.send(json!({"t": "e", "id": "x2", "c": "#445566", "w": 0.01, "pts": [0.3, 0.3, 0.5, 0.4, 0.4, 0.5]})).await;
    a.expect("ok").await;
    let e = b.expect("e").await;
    assert_eq!(e["c"], "#445566");
    assert_eq!(e["w"], 0.01);

    let (_c, _, strokes) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    assert_eq!(strokes.len(), 2);
    s.stop().await;
}

#[tokio::test]
async fn bad_input_is_refused_or_clamped() {
    let s = start(&temp_db("bad")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut v, _, _) = Client::join(&s.addr, &view, None).await;

    for bad in [
        json!({"t": "b", "id": "bad id", "c": "#112233", "w": 0.004}),
        json!({"t": "b", "id": "x", "c": "red", "w": 0.004}),
        json!({"t": "b", "id": "x", "c": "#112233", "w": 0.5}),
        json!({"t": "b", "id": "x", "c": "#112233", "w": 0}),
        json!({"t": "b", "id": "x\"},{\"t\":\"x", "c": "#112233", "w": 0.004}),
        json!({"t": "p", "id": "nope", "pts": [0.1, 0.1, 0.5]}),
        json!({"t": "zz"}),
        json!([1, 2, 3]),
    ] {
        a.send(bad).await;
    }
    a.ws.send(Message::Text("not json".into())).await.unwrap();
    v.quiet().await;

    // Refusals that the author hears about.
    a.send(json!({"t": "s", "id": "odd", "c": "#112233", "w": 0.004, "pts": [0.1, 0.1]})).await;
    assert_eq!(a.expect("no").await["id"], "odd");
    a.send(json!({"t": "s", "id": "empty", "c": "#112233", "w": 0.004, "pts": []})).await;
    assert_eq!(a.expect("no").await["id"], "empty");
    a.send(json!({"t": "e", "id": "ghost", "pts": [0.1, 0.1, 0.5]})).await;
    assert_eq!(a.expect("no").await["id"], "ghost");
    v.quiet().await;

    // Points outside the board are pulled onto it; long decimals are rounded.
    a.send(json!({"t": "s", "id": "wild", "c": "#112233", "w": 0.004, "pts": [-5, 9, 3, 0.123456789, 0.2, 0.5, -0.25, -0.1, 0.5]})).await;
    a.expect("ok").await;
    // The surface is 3 by 3 pages: x -1..2, y -0.5625..1.125.
    assert_eq!(v.expect("e").await["pts"], json!([-1, 1.125, 1, 0.12346, 0.2, 0.5, -0.25, -0.1, 0.5]));

    // Another connection can't feed or finish my live stroke.
    let (mut b, _, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    a.expect("l").await;
    v.expect("l").await;
    a.send(json!({"t": "b", "id": "mine", "c": "#112233", "w": 0.004})).await;
    v.expect("b").await;
    b.expect("b").await;
    b.send(json!({"t": "p", "id": "mine", "pts": [0.1, 0.1, 0.5]})).await;
    b.send(json!({"t": "e", "id": "mine", "c": "#112233", "w": 0.004, "pts": [0.1, 0.1, 0.5]})).await;
    v.quiet().await;

    // A live stroke is abandoned when its author disconnects.
    drop(a);
    assert_eq!(v.expect("u").await["id"], "mine");
    s.stop().await;
}

#[tokio::test]
async fn join_errors() {
    let s = start(&temp_db("join")).await;
    let (edit, _) = new_board(&s.addr).await;

    let mut c = Client::open(&s.addr).await;
    c.send(json!({"t": "join", "key": "x".repeat(24), "who": null})).await;
    assert_eq!(c.expect("err").await["code"], "gone");
    assert!(c.raw().await.is_none(), "the relay closes after an error");

    let mut c = Client::open(&s.addr).await;
    c.send(json!({"t": "join", "key": edit, "who": null})).await;
    assert_eq!(c.expect("err").await["code"], "bad");

    let mut c = Client::open(&s.addr).await;
    c.send(json!({"t": "b", "id": "x", "c": "#112233", "w": 0.004})).await;
    assert_eq!(c.expect("err").await["code"], "bad");
    s.stop().await;
}

#[tokio::test]
async fn flooding_closes_the_connection() {
    let s = start(&temp_db("flood")).await;
    let (edit, _) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    for _ in 0..1500 {
        if a.ws.send(Message::Text(r#"{"t":"u","id":"nothing"}"#.into())).await.is_err() {
            break;
        }
    }
    let mut saw_rate = false;
    while let Some(m) = a.raw().await {
        if m["t"] == "err" {
            assert_eq!(m["code"], "rate");
            saw_rate = true;
        }
    }
    assert!(saw_rate);
    s.stop().await;
}

#[tokio::test]
async fn presence_counts_connections() {
    let s = start(&temp_db("presence")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (v, _, _) = Client::join(&s.addr, &view, None).await;
    let mut last = 0;
    while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_millis(300), a.ws.next()).await {
        if let Message::Text(t) = m {
            let m: Value = serde_json::from_str(&t).unwrap();
            if m["t"] == "n" {
                last = m["n"].as_u64().unwrap();
            }
        }
    }
    assert_eq!(last, 2);
    drop(v);
    let m = a.raw().await.unwrap();
    assert_eq!((m["t"].as_str(), m["n"].as_u64()), (Some("n"), Some(1)));
    s.stop().await;
}

#[tokio::test]
async fn text_annotations() {
    let s = start(&temp_db("text")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut v, _, _) = Client::join(&s.addr, &view, None).await;

    let text = "Load balancer \"LB-1\"\nline two <b>";
    a.send(json!({"t": "s", "id": "t1", "c": "#0a66d8", "w": 0.02, "pts": [0.25, 0.1, 0.5], "txt": text})).await;
    a.expect("ok").await;
    let e = v.expect("e").await;
    assert_eq!(e["txt"], text);
    assert_eq!(e["pts"], json!([0.25, 0.1, 0.5]));
    assert_eq!(e["w"], 0.02);

    // Refused: more than one point, empty text, control characters, too long.
    for (id, pts, txt) in [
        ("t2", json!([0.1, 0.1, 0.5, 0.2, 0.2, 0.5]), "two points".to_string()),
        ("t3", json!([0.1, 0.1, 0.5]), "   ".to_string()),
        ("t4", json!([0.1, 0.1, 0.5]), "bell\u{7}".to_string()),
        ("t5", json!([0.1, 0.1, 0.5]), "x".repeat(501)),
    ] {
        a.send(json!({"t": "s", "id": id, "c": "#0a66d8", "w": 0.02, "pts": pts, "txt": txt})).await;
        assert_eq!(a.expect("no").await["id"], id);
    }
    v.quiet().await;

    // Text is part of the layer: deleted and restored with it, and it survives a restart.
    a.send(json!({"t": "ld", "l": 1})).await;
    a.expect("ld").await;
    v.expect("ld").await;
    drop(a);
    drop(v);
    let db = s.stop().await;
    let s = start(&db).await;
    let (mut a, _, strokes) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    assert!(strokes.is_empty());
    a.send(json!({"t": "lr", "l": 1})).await;
    assert_eq!(a.expect("lr").await["a"][0]["txt"], text);
    // The author can undo it like a stroke.
    a.send(json!({"t": "u", "id": "t1"})).await;
    let (_v, _, strokes) = Client::join(&s.addr, &view, None).await;
    assert!(strokes.is_empty());
    s.stop().await;
}

#[tokio::test]
async fn crumple_shelves_the_sheet_and_brings_it_back() {
    let s = start(&temp_db("crumple")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut a, hello, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    assert_eq!(hello["shelf"], json!([]));
    let (mut v, _, _) = Client::join(&s.addr, &view, None).await;

    // Crumpling a blank sheet does nothing; a viewer can't crumple.
    a.send(json!({"t": "cr"})).await;
    a.quiet().await;
    a.draw("one-a").await;
    a.draw("one-b").await;
    v.expect("e").await;
    v.expect("e").await;
    v.send(json!({"t": "cr"})).await;
    a.quiet().await;

    a.send(json!({"t": "cr"})).await;
    let cr = a.expect("cr").await;
    assert_eq!(cr["shelf"], json!([{"k": 1, "n": 2}]));
    assert!(cr["drop"].is_null());
    assert_eq!(v.expect("cr").await["k"], 1);
    let (_j, hello, strokes) = Client::join(&s.addr, &view, None).await;
    assert!(strokes.is_empty(), "the new sheet is blank");
    assert_eq!(hello["shelf"], json!([{"k": 1, "n": 2}]));

    // A preview of the crumpled sheet, for the one who asks.
    a.send(json!({"t": "sv", "k": 1})).await;
    let sv = a.expect("sv").await;
    assert_eq!(sv["a"].as_array().unwrap().len(), 2);
    v.quiet().await;

    // Draw a second sheet, then go back to the first: they swap.
    a.draw("two-a").await;
    v.expect("e").await;
    a.send(json!({"t": "un", "k": 1})).await;
    let un = a.expect("un").await;
    let ids: Vec<&str> = un["a"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["one-a", "one-b"]);
    assert_eq!(un["shelf"], json!([{"k": 2, "n": 1}]));
    v.expect("un").await;

    // It all survives a restart.
    drop(a);
    drop(v);
    let db = s.stop().await;
    let s = start(&db).await;
    let (mut a, hello, strokes) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    assert_eq!(strokes.len(), 2);
    assert_eq!(hello["shelf"], json!([{"k": 2, "n": 1}]));

    // Bringing a sheet back onto a blank sheet shelves nothing.
    a.send(json!({"t": "cr"})).await;
    assert_eq!(a.expect("cr").await["shelf"], json!([{"k": 2, "n": 1}, {"k": 3, "n": 2}]));
    a.send(json!({"t": "un", "k": 2})).await;
    assert_eq!(a.expect("un").await["shelf"], json!([{"k": 3, "n": 2}]));

    // The shelf holds five; a sixth crumple throws the oldest away.
    for i in 0..5 {
        a.draw(&format!("fill-{i}")).await;
        a.send(json!({"t": "cr"})).await;
        let cr = a.expect("cr").await;
        let shelf = cr["shelf"].as_array().unwrap();
        assert!(shelf.len() <= 5);
        if i == 4 {
            assert_eq!(cr["drop"], 3, "sheet 3 was the oldest");
            assert_eq!(shelf.len(), 5);
            assert!(shelf.iter().all(|s| s["k"] != 3));
        }
    }
    // A thrown-away sheet can't be brought back; its stroke ids are free again.
    a.send(json!({"t": "un", "k": 3})).await;
    a.quiet().await;
    a.draw("one-a").await;
    s.stop().await;
}

#[tokio::test]
async fn a_reconnect_can_finish_the_stroke_its_old_socket_began() {
    let s = start(&temp_db("reconnect")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut old, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut v, _, _) = Client::join(&s.addr, &view, None).await;
    old.send(json!({"t": "b", "id": "half", "c": "#112233", "w": 0.004})).await;
    old.send(json!({"t": "p", "id": "half", "pts": [0.1, 0.1, 0.5]})).await;
    v.expect("b").await;
    v.expect("p").await;
    // The same browser reconnects while the relay still holds the old socket,
    // and sends the finished stroke whole.
    let (mut new, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    new.send(json!({"t": "s", "id": "half", "c": "#112233", "w": 0.004, "pts": [0.1, 0.1, 0.5, 0.3, 0.3, 0.5]})).await;
    assert_eq!(new.expect("ok").await["id"], "half");
    assert_eq!(v.expect("e").await["id"], "half");
    // The old socket going away must not remove it.
    drop(old);
    new.quiet().await;
    v.quiet().await;
    let (_j, _, strokes) = Client::join(&s.addr, &view, None).await;
    assert_eq!(strokes.len(), 1);
    s.stop().await;
}

#[tokio::test]
async fn a_board_has_a_size_budget_and_undo_frees_it() {
    let s = start(&temp_db("budget")).await;
    let (edit, _) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    // 20,000 points of awkward decimals: about 470 KB each once stored.
    let pts: Vec<f64> = (0..60_000).map(|i| f64::from(12345 + i % 7) / 100000.0).collect();
    let mut stored = 0;
    let mut refused = false;
    for i in 0..60 {
        // Stay under the byte-rate limit: this test is about the board budget.
        tokio::time::sleep(Duration::from_millis(1900)).await;
        a.send(json!({"t": "s", "id": format!("big{i}"), "c": "#112233", "w": 0.004, "pts": pts})).await;
        let m = a.next().await;
        if m["t"] == "no" {
            refused = true;
            break;
        }
        assert_eq!(m["t"], "ok", "{m}");
        stored += 1;
    }
    assert!(refused, "the board never filled");
    assert!((30..=40).contains(&stored), "stored {stored} strokes of about 470 KB in 16 MB");
    // Undo one: there is room again.
    a.send(json!({"t": "u", "id": "big0"})).await;
    tokio::time::sleep(Duration::from_millis(1900)).await;
    a.send(json!({"t": "s", "id": "again", "c": "#112233", "w": 0.004, "pts": pts})).await;
    assert_eq!(a.expect("ok").await["id"], "again");
    s.stop().await;
}

#[tokio::test]
async fn other_sites_cannot_use_the_relay() {
    let s = start(&temp_db("origin")).await;
    let mut sock = TcpStream::connect(&s.addr).await.unwrap();
    let req = "POST /api/boards HTTP/1.1\r\nHost: test\r\nOrigin: https://evil.example\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 403"));
    let mut sock = TcpStream::connect(&s.addr).await.unwrap();
    let req = "POST /api/boards HTTP/1.1\r\nHost: test\r\nOrigin: http://test\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 201"));
    s.stop().await;
}

#[tokio::test]
async fn names_and_pings_reach_everyone_and_are_not_stored() {
    let s = start(&temp_db("names")).await;
    let (edit, view) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (mut b, _, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    a.expect("l").await;
    let (mut v, _, _) = Client::join(&s.addr, &view, None).await;

    // A label: spaces are tidied, the colour is lower-cased, everyone hears it.
    a.send(json!({"t": "nm", "n": "  Amber   \"Otter\" ", "c": "#AA5500"})).await;
    for c in [&mut a, &mut b, &mut v] {
        let m = c.expect("nm").await;
        assert_eq!((m["l"].as_u64(), m["n"].as_str(), m["c"].as_str()), (Some(1), Some("Amber \"Otter\""), Some("#aa5500")));
    }
    // Bad labels are ignored.
    a.send(json!({"t": "nm", "n": "", "c": "#aa5500"})).await;
    a.send(json!({"t": "nm", "n": "x".repeat(25), "c": "#aa5500"})).await;
    a.send(json!({"t": "nm", "n": "ok", "c": "red"})).await;
    a.quiet().await;

    // A ping gets a letter, goes to everyone including the sender, and is clamped.
    a.send(json!({"t": "pg", "pts": [0.1, 0.1, 0.5, 5.0, 0.2, 0.5]})).await;
    for c in [&mut a, &mut b, &mut v] {
        let m = c.expect("pg").await;
        assert_eq!((m["l"].as_u64(), m["k"].as_str()), (Some(1), Some("A")));
        assert_eq!(m["pts"], json!([0.1, 0.1, 0.5, 2, 0.2, 0.5]));
    }
    // A second ping straight away from the same person is dropped; another person's gets B.
    a.send(json!({"t": "pg", "pts": [0.1, 0.1, 0.5, 0.2, 0.2, 0.5]})).await;
    b.send(json!({"t": "pg", "pts": [0.1, 0.1, 0.5, 0.2, 0.2, 0.5]})).await;
    let m = a.expect("pg").await;
    assert_eq!((m["l"].as_u64(), m["k"].as_str()), (Some(2), Some("B")));
    // A viewer cannot ping, and one point is not a shape.
    v.send(json!({"t": "pg", "pts": [0.1, 0.1, 0.5, 0.2, 0.2, 0.5]})).await;
    b.expect("pg").await;
    b.quiet().await;

    // A late joiner sees the name in hello, no ping, and nothing was stored.
    let (_c, hello, strokes) = Client::join(&s.addr, &view, None).await;
    assert_eq!(hello["layers"][0]["n"], "Amber \"Otter\"");
    assert_eq!(hello["layers"][0]["c"], "#aa5500");
    assert!(hello["layers"][1]["n"].is_null());
    assert!(strokes.is_empty());
    s.stop().await;
}

#[tokio::test]
async fn presence_lists_the_layers_that_are_connected() {
    let s = start(&temp_db("online")).await;
    let (edit, _) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    let (b, _, _) = Client::join(&s.addr, &edit, Some(WHO_B)).await;
    let mut last = Value::Null;
    while let Ok(Some(Ok(f))) = tokio::time::timeout(Duration::from_millis(300), a.ws.next()).await {
        let Message::Text(t) = f else { continue };
        let m: Value = serde_json::from_str(&t).unwrap();
        if m["t"] == "n" {
            last = m;
        }
    }
    assert_eq!(last["on"], json!([1, 2]));
    drop(b);
    loop {
        let m = a.raw().await.unwrap();
        if m["t"] == "n" {
            assert_eq!(m["on"], json!([1]));
            break;
        }
    }
    s.stop().await;
}

#[tokio::test]
async fn previews_draw_on_the_heavy_budget_even_when_cached() {
    let s = start(&temp_db("svbudget")).await;
    let (edit, _) = new_board(&s.addr).await;
    let (mut a, _, _) = Client::join(&s.addr, &edit, Some(WHO_A)).await;
    a.draw("one").await;
    a.send(json!({"t": "cr"})).await;
    a.expect("cr").await;
    // The crumple took one token of twelve; the first preview builds the cache and every
    // later one is a cache hit, which must cost a token all the same.
    for _ in 0..30 {
        a.send(json!({"t": "sv", "k": 1})).await;
    }
    let mut got = 0;
    while let Some(m) = a.raw().await {
        if m["t"] == "sv" {
            got += 1;
        }
        if got > 12 {
            break;
        }
    }
    assert!((11..=12).contains(&got), "{got} previews answered");
    s.stop().await;
}
