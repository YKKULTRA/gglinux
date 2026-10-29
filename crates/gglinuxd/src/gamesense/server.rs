//! The GameSense HTTP endpoints, on loopback only, and closed to web pages:
//! a request carrying an `Origin` header is refused, and every POST must be
//! `application/json`, which a browser can't send cross-origin without a
//! CORS preflight this server never approves.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gg_gamesense::{proto, Registry};
use serde::de::DeserializeOwned;
use serde_json::json;

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
        .route("/supports_multiple_game_events", get(|| async { Json(json!({})) }))
        .layer(middleware::from_fn(guard))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(shared)
}

async fn guard(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return error(StatusCode::FORBIDDEN, "requests from web pages are not accepted");
    }
    if req.method() == Method::POST {
        let json = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type must be application/json");
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
    apply(&s, &body, Change::Structure, |r, m: proto::GameMetadata, _| r.game_metadata(m))
}

async fn register_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::RegisterEvent, _| r.register_event(e))
}

async fn bind_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::BindEvent, _| r.bind_event(e))
}

async fn game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, e: proto::GameEvent, now| r.game_event(e, now))
}

async fn multiple_game_events(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, m: proto::MultipleEvents, now| r.multiple_events(m, now))
}

async fn game_heartbeat(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, g: proto::GameOnly, now| r.heartbeat(&g.game, now))
}

async fn stop_game(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, g: proto::GameOnly, _| r.stop_game(&g.game))
}

async fn remove_game(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, g: proto::GameOnly, _| r.remove_game(&g.game))
}

async fn remove_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::RemoveEvent, _| r.remove_event(&e.game, &e.event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt;
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

    async fn call(app: &Router, method: &str, path: &str, headers: &[(&str, &str)], body: String) -> (StatusCode, String) {
        let mut req = HttpRequest::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn post(app: &Router, path: &str, body: &str) -> (StatusCode, String) {
        call(app, "POST", path, &[("content-type", "application/json")], body.to_string()).await
    }

    const BIND: &str = r#"{"game":"G","event":"HEALTH","handlers":[{"device-type":"keyboard",
        "zone":"function-keys","mode":"percent","color":{"red":255,"green":0,"blue":0}}]}"#;

    #[tokio::test]
    async fn web_pages_are_refused() {
        let (s, _) = shared();
        let app = router(s);
        let (status, body) = call(&app, "POST", "/game_event",
            &[("content-type", "application/json"), ("origin", "https://example.com")], "{}".into()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("error"));
        let (status, _) = call(&app, "POST", "/game_event", &[("content-type", "text/plain")], "{}".into()).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "a no-cors fetch can only send text/plain");
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
        assert_eq!(post(&app, "/game_metadata", r#"{"game":"G","game_display_name":"Game"}"#).await, (StatusCode::OK, "{}".into()));
        assert_eq!(post(&app, "/bind_game_event", BIND).await.0, StatusCode::OK);
        assert_eq!(post(&app, "/game_event", r#"{"game":"G","event":"HEALTH","data":{"value":100}}"#).await.0, StatusCode::OK);
        let layer = s.registry.lock().unwrap().paint(s.clock.now()).expect("G paints");
        assert_eq!(layer.len(), 12);
        assert_eq!(*seen.lock().unwrap(), vec![Change::Structure, Change::Structure, Change::Live]);
    }

    #[tokio::test]
    async fn mistakes_are_400_with_an_error_message() {
        let (s, seen) = shared();
        let app = router(s);
        let (status, body) = post(&app, "/game_event", "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("\"error\""), "{body}");
        let (status, body) = post(&app, "/game_event", r#"{"game":"G","event":"E","data":{"value":1}}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("unknown game"), "{body}");
        assert!(seen.lock().unwrap().is_empty(), "failed requests change nothing");
    }

    #[tokio::test]
    async fn heartbeats_keep_a_game_up_and_stop_game_ends_it() {
        let (s, _) = shared();
        let app = router(s.clone());
        post(&app, "/bind_game_event", BIND).await;
        post(&app, "/game_event", r#"{"game":"G","event":"HEALTH","data":{"value":50}}"#).await;
        assert_eq!(post(&app, "/game_heartbeat", r#"{"game":"G"}"#).await.0, StatusCode::OK);
        assert_eq!(s.registry.lock().unwrap().active_game(s.clock.now()), Some("G"));
        assert_eq!(post(&app, "/stop_game", r#"{"game":"G"}"#).await.0, StatusCode::OK);
        assert_eq!(s.registry.lock().unwrap().paint(s.clock.now()), None);
    }

    #[tokio::test]
    async fn multiple_events_and_the_capability_probe() {
        let (s, _) = shared();
        let app = router(s.clone());
        post(&app, "/bind_game_event", BIND).await;
        let (status, _) = post(&app, "/multiple_game_events",
            r#"{"game":"G","events":[{"event":"HEALTH","data":{"value":10}},{"event":"HEALTH","data":{"value":90}}]}"#).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(&app, "GET", "/supports_multiple_game_events", &[], String::new()).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "{}"));
    }
}
