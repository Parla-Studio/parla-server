//! End-to-end tests: real router, real SQLite database and content directory in a temp folder.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use parla::{api, build_state, config::Config};
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

struct TestServer {
    app: Router,
    dir: TempDir,
}

struct Res {
    status: StatusCode,
    json: Value,
    text: String,
    headers: axum::http::HeaderMap,
}

impl TestServer {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(tweak: impl FnOnce(&mut Config)) -> Self {
        let dir = TempDir::new().unwrap();
        let mut config = Config {
            database: dir.path().join("db/test.db"),
            content_dir: dir.path().join("content"),
            trust_proxy: false,
            session_days: 30,
            ..Config::default()
        };
        tweak(&mut config);
        // Check the content directory on every request so hot reload is testable without waiting.
        let state = build_state(config, Duration::ZERO).unwrap();
        Self { app: api::router(state), dir }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>, token: Option<&str>, etag: Option<&str>) -> Res {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(e) = etag {
            req = req.header("if-none-match", format!("\"{e}\""));
        }
        let req = req
            .header("content-type", "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        Res { status, json, text, headers }
    }

    async fn get(&self, path: &str, token: Option<&str>) -> Res {
        self.call("GET", path, None, token, None).await
    }

    async fn post(&self, path: &str, body: Value, token: Option<&str>) -> Res {
        self.call("POST", path, Some(body), token, None).await
    }

    async fn register(&self, user: &str) -> String {
        let r = self.post("/api/auth/register", json!({"username": user, "password": "geheim123", "device": "test"}), None).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["token"].as_str().unwrap().to_string()
    }
}

fn today() -> String {
    chrono::Local::now().date_naive().to_string()
}

#[tokio::test]
async fn register_login_logout() {
    let s = TestServer::new();
    let token = s.register("niklas").await;

    let dup = s.post("/api/auth/register", json!({"username": "NIKLAS", "password": "geheim123"}), None).await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert_eq!(dup.json["error"], "username_taken");
    assert_eq!(s.post("/api/auth/register", json!({"username": "x", "password": "geheim123"}), None).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(s.post("/api/auth/register", json!({"username": "anna", "password": "kurz"}), None).await.status, StatusCode::BAD_REQUEST);

    let wrong = s.post("/api/auth/login", json!({"username": "niklas", "password": "falsch!!"}), None).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.json["message"], "Benutzername oder Passwort ist falsch.");

    let login = s.post("/api/auth/login", json!({"username": "Niklas", "password": "geheim123"}), None).await;
    assert_eq!(login.status, StatusCode::OK);
    let second = login.json["token"].as_str().unwrap().to_string();
    assert_ne!(token, second);
    assert_eq!(login.json["user"]["username"], "niklas");

    let me = s.get("/api/me", Some(&second)).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["user"]["username"], "niklas");
    assert_eq!(me.json["settings"]["goal"], 20);

    assert_eq!(s.post("/api/auth/logout", json!({}), Some(&second)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(s.get("/api/me", Some(&second)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(s.get("/api/me", Some(&token)).await.status, StatusCode::OK);
    assert_eq!(s.get("/api/progress", None).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn registration_can_be_closed() {
    let s = TestServer::with(|c| c.registration_open = false);
    let r = s.post("/api/auth/register", json!({"username": "niklas", "password": "geheim123"}), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"], "registration_closed");
}

#[tokio::test]
async fn login_is_rate_limited() {
    let s = TestServer::new();
    s.register("brute").await;
    let mut last = StatusCode::OK;
    for _ in 0..10 {
        last = s.post("/api/auth/login", json!({"username": "brute", "password": "wrong-pass"}), None).await.status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn courses_are_served_with_etags() {
    let s = TestServer::new();
    let list = s.get("/api/courses", None).await;
    assert_eq!(list.status, StatusCode::OK);
    let courses = list.json["courses"].as_array().unwrap();
    assert_eq!(courses.len(), 2);
    assert_eq!(courses[0]["skills"], 21);
    let list_hash = list.json["hash"].as_str().unwrap();
    assert_eq!(s.call("GET", "/api/courses", None, None, Some(list_hash)).await.status, StatusCode::NOT_MODIFIED);

    let en = s.get("/api/courses/en", None).await;
    assert_eq!(en.status, StatusCode::OK);
    assert_eq!(en.json["id"], "en");
    let hash = en.headers["x-course-hash"].to_str().unwrap().to_string();
    assert_eq!(en.headers["etag"].to_str().unwrap(), format!("\"{hash}\""));
    assert_eq!(s.call("GET", "/api/courses/en", None, None, Some(&hash)).await.status, StatusCode::NOT_MODIFIED);
    assert_eq!(s.get("/api/courses/xx", None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn edited_course_file_is_picked_up_without_restart() {
    let s = TestServer::new();
    let before = s.get("/api/courses", None).await.json["hash"].clone();
    let file = s.dir.path().join("content/fr.json");
    let text = std::fs::read_to_string(&file).unwrap().replacen("\"version\": 2", "\"version\": 99", 1);
    assert!(text.contains("\"version\": 99"));
    std::fs::write(&file, text).unwrap();
    filetime::set_file_mtime(&file, filetime::FileTime::from_unix_time(4_000_000_000, 0)).unwrap();

    let list = s.get("/api/courses", None).await;
    assert_ne!(list.json["hash"], before);
    let fr = list.json["courses"].as_array().unwrap().iter().find(|c| c["id"] == "fr").unwrap().clone();
    assert_eq!(fr["version"], 99);

    // A broken file must not take the course offline.
    std::fs::write(&file, "{ broken").unwrap();
    filetime::set_file_mtime(&file, filetime::FileTime::from_unix_time(4_000_000_100, 0)).unwrap();
    let still = s.get("/api/courses/fr", None).await;
    assert_eq!(still.status, StatusCode::OK);
    assert_eq!(still.json["version"], 99);

    // A deleted file removes the course.
    std::fs::remove_file(&file).unwrap();
    assert_eq!(s.get("/api/courses/fr", None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn progress_events_are_idempotent_and_track_accuracy() {
    let s = TestServer::new();
    let token = s.register("lerner").await;
    let day = today();
    let now = chrono::Utc::now().timestamp_millis();
    let events = json!([
        {"id": "evt-000001", "type": "lesson", "course": "en", "skill": "g1", "lesson": 0, "xp": 13, "day": day, "at": now, "correct": 8, "total": 10},
        {"id": "evt-000002", "type": "lesson", "course": "en", "skill": "g1", "lesson": 1, "xp": 12, "day": day},
        {"id": "evt-000003", "type": "lesson", "course": "en", "skill": "g1", "lesson": 2, "xp": 11, "day": day},
        {"id": "evt-bad-001", "type": "lesson", "course": "en", "skill": "g1", "lesson": 99, "xp": 10, "day": day},
        {"id": "evt-bad-002", "type": "lesson", "course": "en", "skill": "g1", "lesson": 0, "xp": 999, "day": day},
        {"id": "evt-bad-003", "type": "lesson", "course": "en", "skill": "g1", "lesson": 0, "xp": 10, "day": day, "correct": 5, "total": 3},
        {"id": "evt-bad-004", "type": "lesson", "course": "xx", "skill": "g1", "lesson": 0, "xp": 10, "day": day},
        {"id": "evt-bad-005", "type": "lesson", "course": "en", "skill": "g1", "lesson": 0, "xp": 10, "day": "2000-01-01"}
    ]);
    let r = s.post("/api/progress/events", json!({"events": events}), Some(&token)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["applied"], 3);
    let rejected = r.json["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 5);
    assert_eq!(rejected[0]["reason"], "lesson out of range");

    let g1 = &r.json["progress"]["courses"]["en"]["g1"];
    assert_eq!(g1["lessons"], 3);
    assert_eq!(g1["reps"], 1);
    assert!(g1["lastPracticed"].as_i64().unwrap() > 0);
    assert_eq!(g1["accuracy"], 0.8);
    assert_eq!(g1["answers"], 10);
    assert_eq!(r.json["progress"]["xp"][&day], 36);

    // Same upload again: nothing changes.
    let again = s.post("/api/progress/events", json!({"events": events}), Some(&token)).await;
    assert_eq!(again.json["applied"], 0);
    assert_eq!(again.json["duplicates"], 3);
    assert_eq!(again.json["progress"]["xp"][&day], 36);

    // Practice strengthens and updates accuracy; reset clears the course but keeps XP.
    s.post(
        "/api/progress/events",
        json!({"events": [{"id": "evt-000004", "type": "practice", "course": "en", "skills": ["g1"], "xp": 10, "day": day, "correct": 10, "total": 10}]}),
        Some(&token),
    )
    .await;
    let snap = s.get("/api/progress", Some(&token)).await.json;
    let g1 = &snap["courses"]["en"]["g1"];
    assert_eq!(g1["reps"], 2);
    assert_eq!(g1["accuracy"], 0.87);
    assert_eq!(g1["answers"], 20);

    s.post("/api/progress/events", json!({"events": [{"id": "evt-000005", "type": "reset", "course": "en"}]}), Some(&token)).await;
    let snap = s.get("/api/progress", Some(&token)).await.json;
    assert!(snap["courses"].as_object().unwrap().is_empty());
    assert_eq!(snap["xp"][&day], 46);

    // Too many events in one request.
    let many: Vec<Value> = (0..201).map(|i| json!({"id": format!("bulk-{i:05}"), "type": "reset", "course": "en"})).collect();
    assert_eq!(s.post("/api/progress/events", json!({"events": many}), Some(&token)).await.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(s.post("/api/progress/events", json!({}), Some(&token)).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn settings_password_and_account_deletion() {
    let s = TestServer::new();
    let token = s.register("settings").await;
    let r = s.call("PUT", "/api/settings", Some(json!({"goal": 30, "sound": false, "course": "fr"})), Some(&token), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["goal"], 30);
    assert_eq!(r.json["sound"], false);
    assert_eq!(r.json["listen"], true);
    assert_eq!(s.call("PUT", "/api/settings", Some(json!({"goal": 7})), Some(&token), None).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(s.call("PUT", "/api/settings", Some(json!({"course": "xx"})), Some(&token), None).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(s.get("/api/me", Some(&token)).await.json["settings"]["course"], "fr");

    let other = s.post("/api/auth/login", json!({"username": "settings", "password": "geheim123"}), None).await.json["token"]
        .as_str()
        .unwrap()
        .to_string();
    let wrong = s.post("/api/auth/password", json!({"oldPassword": "nope-nope", "newPassword": "neuesPasswort"}), Some(&token)).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let ok = s.post("/api/auth/password", json!({"oldPassword": "geheim123", "newPassword": "neuesPasswort"}), Some(&token)).await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT);
    assert_eq!(s.get("/api/me", Some(&other)).await.status, StatusCode::UNAUTHORIZED, "other devices are signed out");
    assert_eq!(s.get("/api/me", Some(&token)).await.status, StatusCode::OK, "this device stays signed in");
    assert_eq!(
        s.post("/api/auth/login", json!({"username": "settings", "password": "neuesPasswort"}), None).await.status,
        StatusCode::OK
    );

    assert_eq!(
        s.call("DELETE", "/api/account", Some(json!({"password": "falsch!!"})), Some(&token), None).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.call("DELETE", "/api/account", Some(json!({"password": "neuesPasswort"})), Some(&token), None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(s.get("/api/me", Some(&token)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.post("/api/auth/login", json!({"username": "settings", "password": "neuesPasswort"}), None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn misc_endpoints() {
    let s = TestServer::new();
    let h = s.get("/api/health", None).await;
    assert_eq!(h.status, StatusCode::OK);
    assert_eq!(h.json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(h.headers["cache-control"], "no-cache");

    let nf = s.get("/api/nope", None).await;
    assert_eq!(nf.status, StatusCode::NOT_FOUND);
    assert_eq!(nf.json["error"], "not_found");

    let root = s.get("/", None).await;
    assert!(root.text.starts_with("Parla server"));

    let bad = s.call("POST", "/api/auth/login", None, None, None).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let garbage = s.app.clone().oneshot(Request::post("/api/auth/login").body(Body::from("{nope")).unwrap()).await.unwrap();
    assert_eq!(garbage.status(), StatusCode::BAD_REQUEST);
}
