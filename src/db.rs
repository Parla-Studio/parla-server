//! SQLite storage. One connection guarded by a mutex is plenty for this app and keeps locking simple.

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

pub type DbResult<T> = rusqlite::Result<T>;

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub created_at: i64,
}

/// Progress in one skill. `accuracy` (0..1) is a running average weighted towards recent sessions;
/// `answers` counts graded answers so far.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SkillProgress {
    pub lessons: i64,
    pub last_practiced: i64,
    pub reps: i64,
    pub accuracy: f64,
    pub answers: i64,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  username TEXT NOT NULL UNIQUE COLLATE NOCASE,
  password_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  token_hash TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device TEXT,
  created_at INTEGER NOT NULL,
  last_used INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_user ON sessions(user_id);
CREATE TABLE IF NOT EXISTS skill_progress (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  course TEXT NOT NULL,
  skill TEXT NOT NULL,
  lessons INTEGER NOT NULL DEFAULT 0,
  last_practiced INTEGER NOT NULL DEFAULT 0,
  reps INTEGER NOT NULL DEFAULT 0,
  accuracy REAL NOT NULL DEFAULT 0,
  answers INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (user_id, course, skill)
);
CREATE TABLE IF NOT EXISTS xp_log (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  day TEXT NOT NULL,
  xp INTEGER NOT NULL,
  PRIMARY KEY (user_id, day)
);
CREATE TABLE IF NOT EXISTS applied_events (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL,
  applied_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, event_id)
);
CREATE TABLE IF NOT EXISTS settings (
  user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  json TEXT NOT NULL
);
PRAGMA user_version = 1;
"#;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Runs `f` in a transaction; rolls back if it returns an error.
    pub fn transaction<T>(&self, f: impl FnOnce(&Transaction) -> DbResult<T>) -> DbResult<T> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    // ------------------------------------------------------------------ users

    /// Returns the new user id, or `None` if the username is taken (case-insensitive).
    pub fn create_user(&self, username: &str, password_hash: &str) -> DbResult<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "INSERT INTO users(username, password_hash, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(username) DO NOTHING",
            params![username, password_hash, now_ms()],
        )?;
        Ok((n > 0).then(|| conn.last_insert_rowid()))
    }

    pub fn find_user(&self, username: &str) -> DbResult<Option<User>> {
        self.user_where("username = ?1", &username)
    }

    pub fn user_by_id(&self, id: i64) -> DbResult<Option<User>> {
        self.user_where("id = ?1", &id)
    }

    fn user_where(&self, condition: &str, value: &dyn rusqlite::ToSql) -> DbResult<Option<User>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT id, username, password_hash, created_at FROM users WHERE {condition}"),
            [value],
            |r| Ok(User { id: r.get(0)?, username: r.get(1)?, password_hash: r.get(2)?, created_at: r.get(3)? }),
        )
        .optional()
    }

    pub fn update_password(&self, user_id: i64, hash: &str) -> DbResult<()> {
        self.conn.lock().unwrap().execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![hash, user_id])?;
        Ok(())
    }

    pub fn delete_user(&self, user_id: i64) -> DbResult<()> {
        self.conn.lock().unwrap().execute("DELETE FROM users WHERE id = ?1", [user_id])?;
        Ok(())
    }

    // ------------------------------------------------------------------ sessions

    pub fn create_session(&self, user_id: i64, token_hash: &str, device: Option<&str>) -> DbResult<()> {
        let now = now_ms();
        self.conn.lock().unwrap().execute(
            "INSERT INTO sessions(token_hash, user_id, device, created_at, last_used) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![token_hash, user_id, device, now],
        )?;
        Ok(())
    }

    /// Looks up a session and slides its expiry; sessions idle longer than `max_idle_ms` are deleted.
    pub fn session_user(&self, token_hash: &str, max_idle_ms: i64) -> DbResult<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, i64)> = conn
            .query_row("SELECT user_id, last_used FROM sessions WHERE token_hash = ?1", [token_hash], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let Some((user_id, last_used)) = row else { return Ok(None) };
        let now = now_ms();
        if now - last_used > max_idle_ms {
            conn.execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?;
            return Ok(None);
        }
        // Only write when it matters, to keep reads cheap.
        if now - last_used > 60_000 {
            conn.execute("UPDATE sessions SET last_used = ?1 WHERE token_hash = ?2", params![now, token_hash])?;
        }
        Ok(Some(user_id))
    }

    pub fn delete_session(&self, token_hash: &str) -> DbResult<()> {
        self.conn.lock().unwrap().execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?;
        Ok(())
    }

    pub fn delete_other_sessions(&self, user_id: i64, keep_token_hash: &str) -> DbResult<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash <> ?2", params![user_id, keep_token_hash])?;
        Ok(())
    }

    pub fn purge_idle_sessions(&self, max_idle_ms: i64) -> DbResult<usize> {
        self.conn.lock().unwrap().execute("DELETE FROM sessions WHERE last_used < ?1", [now_ms() - max_idle_ms])
    }

    pub fn purge_old_events(&self, older_than_ms: i64) -> DbResult<usize> {
        self.conn.lock().unwrap().execute("DELETE FROM applied_events WHERE applied_at < ?1", [now_ms() - older_than_ms])
    }

    // ------------------------------------------------------------------ progress

    /// course -> skill -> progress
    pub fn progress(&self, user_id: i64) -> DbResult<BTreeMap<String, BTreeMap<String, SkillProgress>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT course, skill, lessons, last_practiced, reps, accuracy, answers FROM skill_progress WHERE user_id = ?1",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                SkillProgress { lessons: r.get(2)?, last_practiced: r.get(3)?, reps: r.get(4)?, accuracy: r.get(5)?, answers: r.get(6)? },
            ))
        })?;
        let mut out: BTreeMap<String, BTreeMap<String, SkillProgress>> = BTreeMap::new();
        for row in rows {
            let (course, skill, p) = row?;
            out.entry(course).or_default().insert(skill, p);
        }
        Ok(out)
    }

    /// day (yyyy-MM-dd) -> xp
    pub fn xp(&self, user_id: i64) -> DbResult<BTreeMap<String, i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT day, xp FROM xp_log WHERE user_id = ?1")?;
        let rows = stmt.query_map([user_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect()
    }

    // ------------------------------------------------------------------ settings

    pub fn settings(&self, user_id: i64) -> DbResult<Option<String>> {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT json FROM settings WHERE user_id = ?1", [user_id], |r| r.get(0))
            .optional()
    }

    pub fn save_settings(&self, user_id: i64, json: &str) -> DbResult<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO settings(user_id, json) VALUES (?1, ?2) ON CONFLICT(user_id) DO UPDATE SET json = excluded.json",
            params![user_id, json],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------- transaction helpers

/// Marks an event as applied. Returns false if it was applied before.
pub fn mark_event(tx: &Transaction, user_id: i64, event_id: &str) -> DbResult<bool> {
    let n = tx.execute(
        "INSERT INTO applied_events(user_id, event_id, applied_at) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
        params![user_id, event_id, now_ms()],
    )?;
    Ok(n > 0)
}

pub fn read_skill(tx: &Transaction, user_id: i64, course: &str, skill: &str) -> DbResult<Option<SkillProgress>> {
    tx.query_row(
        "SELECT lessons, last_practiced, reps, accuracy, answers FROM skill_progress WHERE user_id = ?1 AND course = ?2 AND skill = ?3",
        params![user_id, course, skill],
        |r| Ok(SkillProgress { lessons: r.get(0)?, last_practiced: r.get(1)?, reps: r.get(2)?, accuracy: r.get(3)?, answers: r.get(4)? }),
    )
    .optional()
}

pub fn write_skill(tx: &Transaction, user_id: i64, course: &str, skill: &str, p: &SkillProgress) -> DbResult<()> {
    tx.execute(
        "INSERT INTO skill_progress(user_id, course, skill, lessons, last_practiced, reps, accuracy, answers)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(user_id, course, skill) DO UPDATE SET
           lessons = excluded.lessons, last_practiced = excluded.last_practiced, reps = excluded.reps,
           accuracy = excluded.accuracy, answers = excluded.answers",
        params![user_id, course, skill, p.lessons, p.last_practiced, p.reps, p.accuracy, p.answers],
    )?;
    Ok(())
}

pub fn add_xp(tx: &Transaction, user_id: i64, day: &str, xp: i64) -> DbResult<()> {
    tx.execute(
        "INSERT INTO xp_log(user_id, day, xp) VALUES (?1, ?2, ?3) ON CONFLICT(user_id, day) DO UPDATE SET xp = xp + excluded.xp",
        params![user_id, day, xp],
    )?;
    Ok(())
}

pub fn reset_course(tx: &Transaction, user_id: i64, course: &str) -> DbResult<()> {
    tx.execute("DELETE FROM skill_progress WHERE user_id = ?1 AND course = ?2", params![user_id, course])?;
    Ok(())
}
