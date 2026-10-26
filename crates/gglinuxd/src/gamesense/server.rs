// SPDX-License-Identifier: GPL-3.0-or-later
//! The GameSense HTTP endpoints, on loopback only, and closed to web pages:
//! a request carrying an `Origin` header is refused, and every POST must be
//! `application/json`, which a browser can't send cross-origin without a
//! CORS preflight this server never approves.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gg_gamesense::{proto, Registry};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tracing::debug;

use super::{Change, Clock};

/// Largest request body accepted.
pub const BODY_LIMIT: usize = 64 * 1024;

/// What the endpoints share.
#[derive(Clone)]
pub struct Shared {
    pub registry: Arc<Mutex<Registry>>,
    pub clock: Clock,
    /// Called after every accepted request: wake the keyboard, publish, save.
    pub changed: Arc<dyn Fn(Change) + Send + Sync>,
}

/// How much one client can hold, so a local program can't use up the
/// daemon's file descriptors with connections it never finishes.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Connections served at once. More wait in the listen queue until one
    /// closes, rather than being accepted and closed: a game that connects
    /// while the server is full is served late, not refused.
    pub connections: usize,
    /// How long a request's headers can take to arrive, which is also how long
    /// a kept-alive connection can sit idle between requests.
    pub headers: Duration,
    /// How long a whole request can take, body included; past it the answer
    /// is 408 and the connection closes.
    pub request: Duration,
    /// How long a stop gives open requests to finish before their connections
    /// are dropped.
    pub grace: Duration,
}

pub const LIMITS: Limits = Limits {
    connections: 32,
    headers: Duration::from_secs(5),
    request: Duration::from_secs(10),
    grace: Duration::from_millis(500),
};

/// How long to wait after a failed accept (out of file descriptors, say)
/// before trying again.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Serve `app` on `listener` within `limits` until `stop` finishes, then close
/// every connection: idle ones at once, ones mid-request within the grace.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: Limits,
    stop: impl Future<Output = ()>,
) {
    let app = app.layer(middleware::from_fn(
        move |req: Request, next: Next| async move {
            tokio::time::timeout(limits.request, next.run(req))
                .await
                .unwrap_or_else(|_| error(StatusCode::REQUEST_TIMEOUT, "the request took too long"))
        },
    ));
    let (closing, closed) = watch::channel(());
    let mut connections = JoinSet::new();
    let mut stop = pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => break,
            Some(_) = connections.join_next() => {}
            accepted = listener.accept(), if connections.len() < limits.connections => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(connection(stream, app.clone(), limits, closed.clone()));
                }
                Err(e) => {
                    debug!(error = %e, "GameSense server could not accept a connection");
                    tokio::time::sleep(ACCEPT_RETRY).await;
                }
            },
        }
    }
    drop(listener);
    drop(closing);
    let _ = tokio::time::timeout(limits.grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    // Dropping the set aborts what's left, which closes those connections.
}

/// One client connection, until it closes or the server stops.
async fn connection(
    stream: TcpStream,
    app: Router,
    limits: Limits,
    mut closing: watch::Receiver<()>,
) {
    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.headers);
    let mut conn =
        pin!(builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app)));
    tokio::select! {
        result = conn.as_mut() => {
            if let Err(e) = result {
                debug!(error = %e, "GameSense connection ended");
            }
            return;
        }
        _ = closing.changed() => conn.as_mut().graceful_shutdown(),
    }
    let _ = conn.await;
}

pub fn router(shared: Shared) -> Router {
    Router::new()
        .route("/game_metadata", post(game_metadata))
        .route("/register_game_event", post(register_game_event))
        .route("/bind_game_event", post(bind_game_event))
        .route("/game_event", post(game_event))
        .route("/multiple_game_events", post(multiple_game_events))
        .route("/game_heartbeat", post(game_heartbeat))
        .route("/stop_game", post(stop_game))
        .route("/remove_game", post(remove_game))
        .route("/remove_game_event", post(remove_game_event))
        .route(
            "/supports_multiple_game_events",
            get(|| async { Json(json!({})) }),
        )
        .layer(middleware::from_fn(guard))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(shared)
}

async fn guard(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return error(
            StatusCode::FORBIDDEN,
            "requests from web pages are not accepted",
        );
    }
    if req.method() == Method::POST {
        let json = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type must be application/json",
            );
        }
    }
    next.run(req).await
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Parse the body, apply it to the registry, and answer.
fn apply<T: DeserializeOwned>(
    s: &Shared,
    body: &Bytes,
    change: Change,
    f: impl FnOnce(&mut Registry, T, f64) -> Result<(), String>,
) -> Response {
    let req: T = match serde_json::from_slice(body) {
        Ok(req) => req,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
    };
    let result = {
        let mut registry = s.registry.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut registry, req, s.clock.now())
    };
    match result {
        Ok(()) => {
            (s.changed)(change);
            Json(json!({})).into_response()
        }
        Err(message) => error(StatusCode::BAD_REQUEST, &message),
    }
}

async fn game_metadata(State(s): State<Shared>, body: Bytes) -> Response {
    apply(
        &s,
        &body,
        Change::Structure,
        |r, m: proto::GameMetadata, _| r.game_metadata(m),
    )
}

async fn register_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(
        &s,
        &body,
        Change::Structure,
        |r, e: proto::RegisterEvent, _| r.register_event(e),
    )
}

async fn bind_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::BindEvent, _| {
        r.bind_event(e)
    })
}

async fn game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, e: proto::GameEvent, now| {
        r.game_event(e, now)
    })
}

async fn multiple_game_events(State(s): State<Shared>, body: Bytes) -> Response {
    apply(
        &s,
        &body,
        Change::Live,
        |r, m: proto::MultipleEvents, now| r.multiple_events(m, now),
    )
}

async fn game_heartbeat(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, g: proto::GameOnly, now| {
        r.heartbeat(&g.game, now)
    })
}

async fn stop_game(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, g: proto::GameOnly, _| {
        r.stop_game(&g.game)
    })
}

async fn remove_game(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, g: proto::GameOnly, _| {
        r.remove_game(&g.game)
    })
}

async fn remove_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(
        &s,
        &body,
        Change::Structure,
        |r, e: proto::RemoveEvent, _| r.remove_event(&e.game, &e.event),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tower::ServiceExt;

    fn shared() -> (Shared, Arc<Mutex<Vec<Change>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let s = Shared {
            registry: Arc::new(Mutex::new(Registry::default())),
            clock: Clock::start(),
            changed: Arc::new(move |c| log.lock().unwrap().push(c)),
        };
        (s, seen)
    }

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: String,
    ) -> (StatusCode, String) {
        let mut req = HttpRequest::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = app
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn post(app: &Router, path: &str, body: &str) -> (StatusCode, String) {
        call(
            app,
            "POST",
            path,
            &[("content-type", "application/json")],
            body.to_string(),
        )
        .await
    }

    const BIND: &str = r#"{"game":"G","event":"HEALTH","handlers":[{"device-type":"keyboard",
        "zone":"function-keys","mode":"percent","color":{"red":255,"green":0,"blue":0}}]}"#;

    #[tokio::test]
    async fn web_pages_are_refused() {
        let (s, _) = shared();
        let app = router(s);
        let (status, body) = call(
            &app,
            "POST",
            "/game_event",
            &[
                ("content-type", "application/json"),
                ("origin", "https://example.com"),
            ],
            "{}".into(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("error"));
        let (status, _) = call(
            &app,
            "POST",
            "/game_event",
            &[("content-type", "text/plain")],
            "{}".into(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a no-cors fetch can only send text/plain"
        );
        let (status, _) = call(&app, "POST", "/game_event", &[], "{}".into()).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn oversized_bodies_are_refused() {
        let (s, _) = shared();
        let (status, _) = post(&router(s), "/game_event", &" ".repeat(BODY_LIMIT + 1)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn register_bind_and_send_paint_the_keyboard() {
        let (s, seen) = shared();
        let app = router(s.clone());
        assert_eq!(
            post(
                &app,
                "/game_metadata",
                r#"{"game":"G","game_display_name":"Game"}"#
            )
            .await,
            (StatusCode::OK, "{}".into())
        );
        assert_eq!(post(&app, "/bind_game_event", BIND).await.0, StatusCode::OK);
        assert_eq!(
            post(
                &app,
                "/game_event",
                r#"{"game":"G","event":"HEALTH","data":{"value":100}}"#
            )
            .await
            .0,
            StatusCode::OK
        );
        let layer = s
            .registry
            .lock()
            .unwrap()
            .paint(s.clock.now())
            .expect("G paints");
        assert_eq!(layer.len(), 12);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![Change::Structure, Change::Structure, Change::Live]
        );
    }

    #[tokio::test]
    async fn mistakes_are_400_with_an_error_message() {
        let (s, seen) = shared();
        let app = router(s);
        let (status, body) = post(&app, "/game_event", "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("\"error\""), "{body}");
        let (status, body) = post(
            &app,
            "/game_event",
            r#"{"game":"G","event":"E","data":{"value":1}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("unknown game"), "{body}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "failed requests change nothing"
        );
    }

    #[tokio::test]
    async fn heartbeats_keep_a_game_up_and_stop_game_ends_it() {
        let (s, _) = shared();
        let app = router(s.clone());
        post(&app, "/bind_game_event", BIND).await;
        post(
            &app,
            "/game_event",
            r#"{"game":"G","event":"HEALTH","data":{"value":50}}"#,
        )
        .await;
        assert_eq!(
            post(&app, "/game_heartbeat", r#"{"game":"G"}"#).await.0,
            StatusCode::OK
        );
        assert_eq!(
            s.registry.lock().unwrap().active_game(s.clock.now()),
            Some("G")
        );
        assert_eq!(
            post(&app, "/stop_game", r#"{"game":"G"}"#).await.0,
            StatusCode::OK
        );
        assert_eq!(s.registry.lock().unwrap().paint(s.clock.now()), None);
    }

    /// Short limits, so the tests don't wait long.
    const QUICK: Limits = Limits {
        connections: 2,
        headers: Duration::from_millis(200),
        request: Duration::from_millis(400),
        grace: Duration::from_millis(200),
    };

    const PROBE: &[u8] = b"GET /supports_multiple_game_events HTTP/1.1\r\nHost: x\r\n\r\n";

    async fn listen(
        limits: Limits,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(serve(listener, router(shared().0), limits, async {
            let _ = stopped.await;
        }));
        (addr, stop, task)
    }

    /// What the server sends until it closes the connection, or `None` if it
    /// is still open after `wait`.
    async fn closed_within(stream: &mut TcpStream, wait: Duration) -> Option<String> {
        let mut got = Vec::new();
        let read = tokio::time::timeout(wait, async {
            let mut buf = [0u8; 1024];
            while let Ok(n @ 1..) = stream.read(&mut buf).await {
                got.extend_from_slice(&buf[..n]);
            }
        });
        read.await
            .ok()
            .map(|()| String::from_utf8_lossy(&got).into_owned())
    }

    #[tokio::test]
    async fn a_silent_or_idle_connection_is_closed() {
        let (addr, _stop, _task) = listen(QUICK).await;
        let mut silent = TcpStream::connect(addr).await.unwrap();
        assert!(
            closed_within(&mut silent, Duration::from_secs(2))
                .await
                .is_some(),
            "a connection that sends nothing"
        );
        let mut idle = TcpStream::connect(addr).await.unwrap();
        idle.write_all(PROBE).await.unwrap();
        let answer = closed_within(&mut idle, Duration::from_secs(2))
            .await
            .expect("a kept-alive connection left idle");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }

    #[tokio::test]
    async fn a_request_whose_body_never_arrives_is_cut_off() {
        let (addr, _stop, _task) = listen(QUICK).await;
        let mut slow = TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"POST /game_event HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{").await.unwrap();
        let answer = closed_within(&mut slow, Duration::from_secs(2))
            .await
            .expect("closed");
        assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    }

    #[tokio::test]
    async fn connections_beyond_the_cap_wait_their_turn() {
        let (addr, _stop, _task) = listen(Limits {
            headers: Duration::from_secs(10),
            ..QUICK
        })
        .await;
        let first = TcpStream::connect(addr).await.unwrap();
        let _second = TcpStream::connect(addr).await.unwrap();
        let mut third = TcpStream::connect(addr).await.unwrap();
        third.write_all(b"GET /supports_multiple_game_events HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
        assert_eq!(
            closed_within(&mut third, Duration::from_millis(300)).await,
            None,
            "not served while two are open"
        );
        drop(first);
        let answer = closed_within(&mut third, Duration::from_secs(2))
            .await
            .expect("served once one closes");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }

    #[tokio::test]
    async fn stopping_closes_open_connections() {
        let (addr, stop, task) = listen(Limits {
            headers: Duration::from_secs(10),
            ..QUICK
        })
        .await;
        let mut silent = TcpStream::connect(addr).await.unwrap();
        let mut half = TcpStream::connect(addr).await.unwrap();
        half.write_all(b"POST /game_event HTTP/1.1\r\nHost: x\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.send(()).unwrap();
        assert!(
            closed_within(&mut silent, Duration::from_secs(1))
                .await
                .is_some(),
            "a silent connection"
        );
        assert!(
            closed_within(&mut half, Duration::from_secs(1))
                .await
                .is_some(),
            "a half-sent request"
        );
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the server task ends")
            .unwrap();
    }

    #[tokio::test]
    async fn multiple_events_and_the_capability_probe() {
        let (s, _) = shared();
        let app = router(s.clone());
        post(&app, "/bind_game_event", BIND).await;
        let (status, _) = post(&app, "/multiple_game_events",
            r#"{"game":"G","events":[{"event":"HEALTH","data":{"value":10}},{"event":"HEALTH","data":{"value":90}}]}"#).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(
            &app,
            "GET",
            "/supports_multiple_game_events",
            &[],
            String::new(),
        )
        .await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "{}"));
    }
}
