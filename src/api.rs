//! HTTP API used by the Android app. Bodies are JSON; authenticated endpoints expect
//! `Authorization: Bearer <token>`. Error responses are `{"error": code, "message": German text}`.
//!
//! ```text
//! GET    /api/health
//! POST   /api/auth/register      {username, password, device?}      -> 201 {token, user}
//! POST   /api/auth/login         {username, password, device?}      -> {token, user}
//! POST   /api/auth/logout                                           -> 204
//! POST   /api/auth/password      {oldPassword, newPassword}         -> 204 (signs out other devices)
//! DELETE /api/account            {password}                         -> 204
//! GET    /api/me                                                    -> {user, settings}
//! PUT    /api/settings           {goal?, sound?, listen?, course?}  -> settings
//! GET    /api/courses                                               -> {hash, courses:[{id,name,flag,version,hash,skills}]}
//! GET    /api/courses/{id}                                          -> course document (ETag = hash)
//! GET    /api/progress                                              -> {courses, xp, serverTime}
//! POST   /api/progress/events    {events:[...]}                     -> {applied, duplicates, rejected, progress}
//! ```

use crate::auth::{self, RateLimiter};
use crate::config::Config;
use crate::content::CourseRepository;
use crate::db::{Db, User};
use crate::progress::{ProgressService, MAX_EVENTS_PER_REQUEST};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_BODY: usize = 256 * 1024;
const GOALS: [i64; 4] = [10, 20, 30, 50];

// ---------------------------------------------------------------------- state

pub struct Inner {
    pub config: Config,
    pub db: Arc<Db>,
    pub courses: Arc<CourseRepository>,
    pub progress: ProgressService,
    login_limiter: RateLimiter,
    register_limiter: RateLimiter,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(config: Config, db: Arc<Db>, courses: Arc<CourseRepository>) -> Self {
        let progress = ProgressService::new(db.clone(), courses.clone());
        Self(Arc::new(Inner {
            config,
            db,
            courses,
            progress,
            login_limiter: RateLimiter::new(8, Duration::from_secs(15 * 60)),
            register_limiter: RateLimiter::new(10, Duration::from_secs(60 * 60)),
        }))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/password", post(change_password))
        .route("/api/account", delete(delete_account))
        .route("/api/me", get(me))
        .route("/api/settings", put(update_settings))
        .route("/api/courses", get(list_courses))
        .route("/api/courses/{id}", get(course_document))
        .route("/api/progress", get(get_progress))
        .route("/api/progress/events", post(post_events))
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(axum::middleware::map_response(no_cache))
        .with_state(state)
}

async fn no_cache(mut res: Response) -> Response {
    res.headers_mut().entry(CACHE_CONTROL).or_insert(HeaderValue::from_static("no-cache"));
    res
}

// ---------------------------------------------------------------------- errors

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }
    fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    fn server() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "server", "Serverfehler. Bitte später erneut versuchen.")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.code, "message": self.message }))).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("database error: {e}");
        Self::server()
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        tracing::error!("background task failed: {e}");
        Self::server()
    }
}

type ApiResult = Result<Response, ApiError>;

// ---------------------------------------------------------------------- helpers

/// Client address: first X-Forwarded-For entry when behind a trusted proxy, else the socket peer.
pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if state.config.trust_proxy {
            if let Some(first) = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                return Ok(ClientIp(first.to_string()));
            }
        }
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string());
        Ok(ClientIp(peer.unwrap_or_else(|| "unknown".into())))
    }
}

struct Auth {
    user_id: i64,
    token_hash: String,
}

fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Auth, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.len() >= 13)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "Bitte anmelden."))?;
    let token_hash = auth::sha256_hex(token);
    match state.db.session_user(&token_hash, state.config.session_max_idle_ms())? {
        Some(user_id) => Ok(Auth { user_id, token_hash }),
        None => Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "Deine Sitzung ist abgelaufen. Bitte erneut anmelden.")),
    }
}

/// Parses a JSON object body; an empty body counts as `{}`.
fn body_object(body: &Bytes) -> Result<Map<String, Value>, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(ApiError::bad("bad_json", "Ungültige Anfrage.")),
    }
}

fn str_field<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

fn user_json(u: &User) -> Value {
    json!({ "id": u.id, "username": u.username, "createdAt": u.created_at })
}

fn json_status(status: StatusCode, v: Value) -> Response {
    (status, Json(v)).into_response()
}

fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn valid_username(u: &str) -> bool {
    (3..=32).contains(&u.len()) && u.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn check_password(p: Option<&str>) -> Result<String, ApiError> {
    match p {
        Some(p) if p.chars().count() >= 8 && p.len() <= 200 => Ok(p.to_string()),
        _ => Err(ApiError::bad("weak_password", "Das Passwort muss mindestens 8 Zeichen lang sein.")),
    }
}

async fn hash_password(password: String) -> Result<String, ApiError> {
    Ok(tokio::task::spawn_blocking(move || auth::hash_password(&password)).await?)
}

async fn verify_password(password: String, hash: String) -> Result<bool, ApiError> {
    Ok(tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash)).await?)
}

fn new_session(state: &AppState, user: &User, device: Option<&str>) -> Result<Value, ApiError> {
    let token = auth::new_token();
    let device: Option<String> = device.map(|d| d.chars().take(100).collect());
    state.db.create_session(user.id, &auth::sha256_hex(&token), device.as_deref())?;
    Ok(json!({ "token": token, "user": user_json(user) }))
}

// ---------------------------------------------------------------------- handlers: misc

async fn health() -> Response {
    Json(json!({ "ok": true, "version": VERSION })).into_response()
}

async fn fallback(uri: Uri) -> Response {
    if uri.path().starts_with("/api/") || uri.path() == "/api" {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", "Unbekannter Endpunkt.").into_response()
    } else {
        ([(CONTENT_TYPE, "text/plain; charset=utf-8")], format!("Parla server {VERSION}\n")).into_response()
    }
}

// ---------------------------------------------------------------------- handlers: account

async fn register(State(state): State<AppState>, ClientIp(ip): ClientIp, body: Bytes) -> ApiResult {
    if !state.config.registration_open {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "registration_closed", "Die Registrierung ist auf diesem Server deaktiviert."));
    }
    if state.register_limiter.is_blocked(&ip) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Zu viele Registrierungen. Bitte später erneut versuchen."));
    }
    let body = body_object(&body)?;
    let username = str_field(&body, "username").map(str::trim).unwrap_or("");
    if !valid_username(username) {
        return Err(ApiError::bad(
            "invalid_username",
            "Benutzername: 3–32 Zeichen, nur Buchstaben, Ziffern, Punkt, Minus und Unterstrich.",
        ));
    }
    let password = check_password(str_field(&body, "password"))?;
    let hash = hash_password(password).await?;
    let Some(id) = state.db.create_user(username, &hash)? else {
        return Err(ApiError::new(StatusCode::CONFLICT, "username_taken", "Dieser Benutzername ist schon vergeben."));
    };
    state.register_limiter.record_failure(&ip); // counts registrations per address
    tracing::info!("new account '{username}' from {ip}");
    let user = state.db.user_by_id(id)?.ok_or_else(ApiError::server)?;
    Ok(json_status(StatusCode::CREATED, new_session(&state, &user, str_field(&body, "device"))?))
}

async fn login(State(state): State<AppState>, ClientIp(ip): ClientIp, body: Bytes) -> ApiResult {
    let body = body_object(&body)?;
    let (Some(username), Some(password)) = (str_field(&body, "username").map(str::trim), str_field(&body, "password")) else {
        return Err(ApiError::bad("missing_fields", "Benutzername und Passwort angeben."));
    };
    let key = format!("{ip}|{}", username.to_lowercase());
    if state.login_limiter.is_blocked(&key) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Zu viele Fehlversuche. Bitte in 15 Minuten erneut versuchen."));
    }
    let user = state.db.find_user(username)?;
    let stored = user.as_ref().map_or_else(|| auth::DUMMY_HASH.clone(), |u| u.password_hash.clone());
    let ok = verify_password(password.to_string(), stored).await?;
    match user {
        Some(user) if ok => {
            state.login_limiter.reset(&key);
            Ok(Json(new_session(&state, &user, str_field(&body, "device"))?).into_response())
        }
        _ => {
            state.login_limiter.record_failure(&key);
            Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid_credentials", "Benutzername oder Passwort ist falsch."))
        }
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    state.db.delete_session(&a.token_hash)?;
    Ok(no_content())
}

async fn change_password(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    let body = body_object(&body)?;
    let user = state.db.user_by_id(a.user_id)?.ok_or_else(ApiError::server)?;
    let old = str_field(&body, "oldPassword").unwrap_or("").to_string();
    if !verify_password(old, user.password_hash.clone()).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "wrong_password", "Das aktuelle Passwort ist falsch."));
    }
    let next = check_password(str_field(&body, "newPassword"))?;
    let hash = hash_password(next).await?;
    state.db.update_password(user.id, &hash)?;
    state.db.delete_other_sessions(user.id, &a.token_hash)?;
    Ok(no_content())
}

async fn delete_account(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    let body = body_object(&body)?;
    let user = state.db.user_by_id(a.user_id)?.ok_or_else(ApiError::server)?;
    let password = str_field(&body, "password").unwrap_or("").to_string();
    if !verify_password(password, user.password_hash.clone()).await? {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "wrong_password", "Das Passwort ist falsch."));
    }
    state.db.delete_user(user.id)?;
    tracing::info!("deleted account '{}'", user.username);
    Ok(no_content())
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    let user = state.db.user_by_id(a.user_id)?.ok_or_else(ApiError::server)?;
    Ok(Json(json!({ "user": user_json(&user), "settings": settings(&state, a.user_id)? })).into_response())
}

// ---------------------------------------------------------------------- handlers: settings

fn settings(state: &AppState, user_id: i64) -> Result<Map<String, Value>, ApiError> {
    let mut s = Map::new();
    s.insert("goal".into(), json!(20));
    s.insert("sound".into(), json!(true));
    s.insert("listen".into(), json!(true));
    if let Some(stored) = state.db.settings(user_id)? {
        if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&stored) {
            s.extend(m);
        }
    }
    Ok(s)
}

async fn update_settings(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    let body = body_object(&body)?;
    let mut s = settings(&state, a.user_id)?;
    if let Some(goal) = body.get("goal") {
        match goal.as_i64().filter(|g| GOALS.contains(g)) {
            Some(g) => {
                s.insert("goal".into(), json!(g));
            }
            None => return Err(ApiError::bad("invalid_goal", "Tagesziel muss 10, 20, 30 oder 50 sein.")),
        }
    }
    for key in ["sound", "listen"] {
        if let Some(b) = body.get(key).and_then(Value::as_bool) {
            s.insert(key.into(), json!(b));
        }
    }
    if let Some(course) = body.get("course") {
        match course {
            Value::Null => {
                s.insert("course".into(), Value::Null);
            }
            Value::String(c) if state.courses.get(c).is_some() => {
                s.insert("course".into(), json!(c));
            }
            _ => return Err(ApiError::bad("unknown_course", "Unbekannter Kurs.")),
        }
    }
    state.db.save_settings(a.user_id, &Value::Object(s.clone()).to_string())?;
    Ok(Json(Value::Object(s)).into_response())
}

// ---------------------------------------------------------------------- handlers: content

fn etag_matches(headers: &HeaderMap, hash: &str) -> bool {
    headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim().trim_start_matches("W/").trim_matches('"') == hash))
}

fn not_modified(hash: &str) -> Response {
    (StatusCode::NOT_MODIFIED, [(ETAG, format!("\"{hash}\""))]).into_response()
}

async fn list_courses(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let courses = state.courses.list();
    let mut all = String::new();
    let list: Vec<Value> = courses
        .iter()
        .map(|c| {
            all.push_str(&format!("{}:{};", c.id, c.hash));
            json!({
                "id": c.id, "name": c.name, "flag": c.flag, "version": c.version,
                "hash": c.hash, "skills": c.lessons_per_skill.len(),
            })
        })
        .collect();
    let hash = auth::sha256_hex(&all)[..16].to_string();
    if etag_matches(&headers, &hash) {
        return not_modified(&hash);
    }
    ([(ETAG, format!("\"{hash}\""))], Json(json!({ "hash": hash, "courses": list }))).into_response()
}

async fn course_document(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let course = state
        .courses
        .get(&id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_course", "Unbekannter Kurs."))?;
    if etag_matches(&headers, &course.hash) {
        return Ok(not_modified(&course.hash));
    }
    Ok((
        [
            (CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
            (ETAG, format!("\"{}\"", course.hash)),
            (axum::http::HeaderName::from_static("x-course-hash"), course.hash.clone()),
        ],
        course.json.clone(),
    )
        .into_response())
}

// ---------------------------------------------------------------------- handlers: progress

async fn get_progress(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    Ok(Json(state.progress.snapshot(a.user_id)?).into_response())
}

async fn post_events(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let a = authenticate(&state, &headers)?;
    let body = body_object(&body)?;
    let events = body
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad("missing_events", "Feld 'events' fehlt."))?;
    if events.len() > MAX_EVENTS_PER_REQUEST {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_events",
            format!("Höchstens {MAX_EVENTS_PER_REQUEST} Ereignisse pro Anfrage."),
        ));
    }
    let r = state.progress.apply(a.user_id, events)?;
    if !r.rejected.is_empty() {
        tracing::warn!("user {}: rejected {} event(s): {:?}", a.user_id, r.rejected.len(), r.rejected);
    }
    Ok(Json(json!({
        "applied": r.applied,
        "duplicates": r.duplicates,
        "rejected": r.rejected,
        "progress": state.progress.snapshot(a.user_id)?,
    }))
    .into_response())
}
