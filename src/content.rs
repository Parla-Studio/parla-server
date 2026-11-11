//! Course files (`<id>.json`) in the content directory, reloaded automatically when they change.
//!
//! This is what lets new lessons reach the app without an app update: edit or add a file, and the
//! next content check from the app sees a new hash and downloads it. A file that fails validation is
//! logged and ignored while the previously loaded version stays online. `"published": false` hides a
//! course from the app (drafts).

use crate::auth::sha256_hex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

include!(concat!(env!("OUT_DIR"), "/bundled_courses.rs"));

#[derive(Debug, Clone)]
pub struct Course {
    pub id: String,
    pub name: String,
    pub flag: String,
    pub version: i64,
    pub hash: String,
    pub published: bool,
    pub order: i64,
    /// The exact document served to the app.
    pub json: String,
    /// Skills in course order with their number of lessons.
    pub lessons_per_skill: Vec<(String, usize)>,
}

impl Course {
    pub fn lessons(&self, skill: &str) -> Option<usize> {
        self.lessons_per_skill.iter().find(|(id, _)| id == skill).map(|(_, n)| *n)
    }
}

#[derive(Default)]
struct State {
    courses: HashMap<String, Arc<Course>>,
    stamps: HashMap<PathBuf, (SystemTime, u64)>,
    last_check: Option<Instant>,
}

pub struct CourseRepository {
    dir: PathBuf,
    check_interval: Duration,
    state: Mutex<State>,
}

impl CourseRepository {
    /// Opens the content directory; if it does not exist it is created and seeded with the built-in courses.
    pub fn open(dir: &Path, check_interval: Duration) -> anyhow::Result<Self> {
        if !dir.is_dir() {
            fs::create_dir_all(dir)?;
            for (name, text) in BUNDLED_COURSES {
                fs::write(dir.join(name), text)?;
                tracing::info!("installed bundled course {name}");
            }
        }
        let repo = Self { dir: dir.to_path_buf(), check_interval, state: Mutex::new(State::default()) };
        repo.reload(true);
        Ok(repo)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Published courses in display order.
    pub fn list(&self) -> Vec<Arc<Course>> {
        self.reload_if_due();
        let state = self.state.lock().unwrap();
        let mut out: Vec<_> = state.courses.values().filter(|c| c.published).cloned().collect();
        out.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Any loaded course, published or not (progress for a hidden course is still accepted).
    pub fn get(&self, id: &str) -> Option<Arc<Course>> {
        self.reload_if_due();
        self.state.lock().unwrap().courses.get(id).cloned()
    }

    fn reload_if_due(&self) {
        let due = {
            let state = self.state.lock().unwrap();
            state.last_check.is_none_or(|t| t.elapsed() >= self.check_interval)
        };
        if due {
            self.reload(false);
        }
    }

    /// Rescans the directory; only files whose modification time or size changed are parsed again.
    pub fn reload(&self, force: bool) {
        let mut state = self.state.lock().unwrap();
        state.last_check = Some(Instant::now());
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("cannot scan content directory {}: {e}", self.dir.display());
                return;
            }
        };
        let mut seen = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
            seen.insert(path.clone(), stamp);
            if !force && state.stamps.get(&path) == Some(&stamp) {
                continue;
            }
            let expected = path.file_stem().unwrap().to_string_lossy().into_owned();
            let result = fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| parse(Some(&expected), &text));
            match result {
                Ok(course) => {
                    match state.courses.get(&course.id) {
                        None => tracing::info!("loaded course '{}' ({} skills, hash {})", course.id, course.lessons_per_skill.len(), course.hash),
                        Some(old) if old.hash != course.hash => {
                            tracing::info!("updated course '{}' (hash {} -> {})", course.id, old.hash, course.hash)
                        }
                        _ => {}
                    }
                    state.courses.insert(course.id.clone(), Arc::new(course));
                }
                Err(e) => {
                    let keep = if state.courses.contains_key(&expected) { " (keeping previous version)" } else { "" };
                    tracing::warn!("ignoring invalid course file {}: {e}{keep}", path.display());
                }
            }
        }
        // Courses whose file was deleted disappear.
        let gone: Vec<PathBuf> = state.stamps.keys().filter(|p| !seen.contains_key(*p)).cloned().collect();
        for path in gone {
            let id = path.file_stem().unwrap().to_string_lossy().into_owned();
            if state.courses.remove(&id).is_some() {
                tracing::info!("removed course '{id}' (file deleted)");
            }
        }
        state.stamps = seen;
    }
}

fn valid_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn req_str<'a>(v: &'a Value, key: &str, field: &str) -> Result<&'a str, String> {
    match v.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(format!("{field} is required")),
    }
}

fn non_empty_strings(v: Option<&Value>) -> bool {
    matches!(v.and_then(Value::as_array), Some(a) if !a.is_empty() && a.iter().all(|x| x.as_str().is_some_and(|s| !s.trim().is_empty())))
}

/// Parses and validates a course document; the error is a readable reason.
pub fn parse(expected_id: Option<&str>, text: &str) -> Result<Course, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    if !root.is_object() {
        return Err("course must be a JSON object".into());
    }
    let id = req_str(&root, "id", "id")?;
    if !valid_id(id) {
        return Err("id must match [a-z0-9][a-z0-9_-]{0,31}".into());
    }
    if let Some(exp) = expected_id {
        if exp != id {
            return Err(format!("id '{id}' does not match file name '{exp}'"));
        }
    }
    let name = req_str(&root, "name", "name")?;
    req_str(&root, "tts", "tts")?;
    let lesson_size = root.get("lessonSize").map(|v| v.as_i64().unwrap_or(0)).unwrap_or(4);
    if !(1..=20).contains(&lesson_size) {
        return Err("lessonSize must be 1..20".into());
    }
    let skills = match root.get("skills").and_then(Value::as_array) {
        Some(s) if !s.is_empty() => s,
        _ => return Err("skills must be a non-empty list".into()),
    };
    let mut lessons: Vec<(String, usize)> = Vec::new();
    for (i, skill) in skills.iter().enumerate() {
        let at = format!("skills[{i}]");
        if !skill.is_object() {
            return Err(format!("{at} must be an object"));
        }
        let sid = req_str(skill, "id", &format!("{at}.id"))?;
        if !valid_id(sid) {
            return Err(format!("{at}.id must match [a-z0-9][a-z0-9_-]{{0,31}}"));
        }
        if lessons.iter().any(|(s, _)| s == sid) {
            return Err(format!("duplicate skill id '{sid}'"));
        }
        req_str(skill, "title", &format!("{at}.title"))?;
        if skill.get("row").and_then(Value::as_i64).is_none() {
            return Err(format!("{at}.row is required"));
        }
        if skill.get("level").is_some_and(|l| !l.is_string()) {
            return Err(format!("{at}.level must be a string such as \"A2\""));
        }
        let sentences = match skill.get("sentences").and_then(Value::as_array) {
            Some(s) if !s.is_empty() => s,
            _ => return Err(format!("{at}.sentences must be a non-empty list")),
        };
        for (j, s) in sentences.iter().enumerate() {
            if !non_empty_strings(s.get("t")) || !non_empty_strings(s.get("de")) {
                return Err(format!("{at}.sentences[{j}] needs non-empty 't' and 'de' lists"));
            }
        }
        if let Some(words) = skill.get("words") {
            let ok = words.as_array().is_some_and(|ws| {
                ws.iter().all(|w| w.as_array().is_some_and(|p| p.len() == 2 && p.iter().all(Value::is_string)))
            });
            if !ok {
                return Err(format!("{at}.words must be a list of [\"target\", \"german\"] pairs"));
            }
        }
        let n = sentences.len().div_ceil(lesson_size as usize);
        lessons.push((sid.to_string(), n));
    }
    Ok(Course {
        id: id.to_string(),
        name: name.to_string(),
        flag: root.get("flag").and_then(Value::as_str).unwrap_or("").to_string(),
        version: root.get("version").and_then(Value::as_i64).unwrap_or(1),
        hash: sha256_hex(text)[..16].to_string(),
        published: root.get("published").and_then(Value::as_bool) != Some(false),
        order: root.get("order").and_then(Value::as_i64).unwrap_or(100),
        json: text.to_string(),
        lessons_per_skill: lessons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_courses_are_valid() {
        assert!(BUNDLED_COURSES.len() >= 2);
        for (name, text) in BUNDLED_COURSES {
            let id = name.trim_end_matches(".json");
            let c = parse(Some(id), text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(c.lessons_per_skill.len(), 21, "{name}");
            assert_eq!(c.lessons("g1"), Some(3));
            assert_eq!(c.lessons("wor"), Some(3));
        }
    }

    #[test]
    fn invalid_courses_are_rejected() {
        assert!(parse(Some("en"), r#"{"id":"fr","name":"x","tts":"fr","skills":[]}"#).is_err());
        assert!(parse(Some("en"), r#"{"id":"en","name":"x","tts":"en","skills":[]}"#).is_err());
        assert!(parse(Some("en"), r#"{"id":"en","name":"x","tts":"en","skills":[{"id":"a","title":"A","row":0,"sentences":[{"t":[],"de":["x"]}]}]}"#).is_err());
        assert!(parse(Some("en"), r#"{"id":"en","name":"x","tts":"en","skills":[{"id":"a","title":"A","row":0,"level":2,"sentences":[{"t":["a"],"de":["b"]}]}]}"#).is_err());
        let ok = parse(
            Some("en"),
            r#"{"id":"en","name":"x","tts":"en","lessonSize":2,"skills":[{"id":"a","title":"A","row":0,
               "sentences":[{"t":["a"],"de":["b"]},{"t":["a"],"de":["b"]},{"t":["a"],"de":["b"]}]}]}"#,
        )
        .unwrap();
        assert_eq!(ok.lessons("a"), Some(2));
        assert!(ok.published);
    }
}
