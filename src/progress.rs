//! Learning events sent by the app.
//!
//! The app records an event whenever a lesson or practice session finishes (also offline) and uploads
//! its queue later. Every event carries a client-generated id, so uploading the same event twice is
//! harmless. The server is the source of truth for progress.

use crate::content::CourseRepository;
use crate::db::{self, Db, DbResult, SkillProgress};
use chrono::{Local, NaiveDate};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const MAX_EVENTS_PER_REQUEST: usize = 200;
pub const MAX_XP_PER_EVENT: i64 = 30;
pub const MAX_ANSWERS_PER_EVENT: i64 = 100;
/// Weight of the newest session in the running accuracy. Must match ProgressLogic.blend in the app.
pub const ACCURACY_WEIGHT: f64 = 0.35;

#[derive(Debug, Default)]
pub struct ApplyResult {
    pub applied: usize,
    pub duplicates: usize,
    pub rejected: Vec<Value>,
}

/// Running accuracy: the first session sets it, later sessions move it by [`ACCURACY_WEIGHT`].
pub fn blend(accuracy: f64, answers: i64, correct: i64, total: i64) -> (f64, i64) {
    if total <= 0 {
        return (accuracy, answers);
    }
    let session = correct as f64 / total as f64;
    let next = if answers == 0 { session } else { accuracy * (1.0 - ACCURACY_WEIGHT) + session * ACCURACY_WEIGHT };
    (next, (answers + total).min(1_000_000))
}

fn event_id_ok(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// Integer field; JSON numbers like `3.0` count as integers too.
fn int(e: &Map<String, Value>, key: &str) -> Option<i64> {
    match e.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        _ => None,
    }
}

fn string<'a>(e: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    e.get(key).and_then(Value::as_str)
}

pub struct ProgressService {
    db: Arc<Db>,
    courses: Arc<CourseRepository>,
}

impl ProgressService {
    pub fn new(db: Arc<Db>, courses: Arc<CourseRepository>) -> Self {
        Self { db, courses }
    }

    pub fn apply(&self, user_id: i64, events: &[Value]) -> DbResult<ApplyResult> {
        let mut result = ApplyResult::default();
        self.db.transaction(|tx| {
            for raw in events {
                let Some(e) = raw.as_object() else {
                    result.rejected.push(json!({"id": "", "reason": "event must be an object"}));
                    continue;
                };
                let id = string(e, "id").unwrap_or("");
                if let Err(reason) = self.validate(e) {
                    result.rejected.push(json!({"id": id, "reason": reason}));
                    continue;
                }
                if !db::mark_event(tx, user_id, id)? {
                    result.duplicates += 1;
                    continue;
                }
                self.apply_one(tx, user_id, e)?;
                result.applied += 1;
            }
            Ok(())
        })?;
        Ok(result)
    }

    fn validate(&self, e: &Map<String, Value>) -> Result<(), String> {
        if !string(e, "id").is_some_and(event_id_ok) {
            return Err("invalid id".into());
        }
        let ty = string(e, "type").unwrap_or("");
        let course = self.courses.get(string(e, "course").unwrap_or("")).ok_or("unknown course")?;
        if ty != "reset" {
            match int(e, "xp") {
                Some(xp) if (0..=MAX_XP_PER_EVENT).contains(&xp) => {}
                _ => return Err(format!("xp must be 0..{MAX_XP_PER_EVENT}")),
            }
            let (correct, total) = (int(e, "correct"), int(e, "total"));
            if correct.is_some() || total.is_some() {
                match (correct, total) {
                    (Some(c), Some(t)) if (0..=MAX_ANSWERS_PER_EVENT).contains(&t) && (0..=t).contains(&c) => {}
                    _ => return Err(format!("correct/total must satisfy 0 <= correct <= total <= {MAX_ANSWERS_PER_EVENT}")),
                }
            }
            let day = NaiveDate::parse_from_str(string(e, "day").unwrap_or(""), "%Y-%m-%d").map_err(|_| "day must be yyyy-MM-dd")?;
            // The app uses the phone's local date: allow time zones and offline queues, reject nonsense.
            let today = Local::now().date_naive();
            if day > today + chrono::Days::new(1) || day < today - chrono::Days::new(366) {
                return Err("day out of range".into());
            }
        }
        match ty {
            "lesson" => {
                let lessons = string(e, "skill").and_then(|s| course.lessons(s)).ok_or("unknown skill")?;
                match int(e, "lesson") {
                    Some(l) if l >= 0 && (l as usize) < lessons => {}
                    _ => return Err("lesson out of range".into()),
                }
            }
            "practice" => {
                let skills = e.get("skills").and_then(Value::as_array).filter(|s| !s.is_empty() && s.len() <= 20);
                let skills = skills.ok_or("skills must be a list of 1..20 skill ids")?;
                if !skills.iter().all(|s| s.as_str().is_some_and(|id| course.lessons(id).is_some())) {
                    return Err("unknown skill in practice".into());
                }
            }
            "reset" => {}
            _ => return Err("unknown type".into()),
        }
        Ok(())
    }

    fn apply_one(&self, tx: &rusqlite::Transaction, user_id: i64, e: &Map<String, Value>) -> DbResult<()> {
        let course_id = string(e, "course").unwrap();
        let course = self.courses.get(course_id).expect("validated");
        let now = db::now_ms();
        let at = int(e, "at").filter(|&a| a > 0 && a <= now).unwrap_or(now);
        let correct = int(e, "correct").unwrap_or(0);
        let total = int(e, "total").unwrap_or(0);

        match string(e, "type").unwrap() {
            "lesson" => {
                let skill = string(e, "skill").unwrap();
                let lesson_count = course.lessons(skill).unwrap() as i64;
                let lesson = int(e, "lesson").unwrap();
                let old = db::read_skill(tx, user_id, course_id, skill)?.unwrap_or_default();
                let was_done = old.lessons >= lesson_count;
                let lessons = old.lessons.max(lesson + 1);
                let mut reps = old.reps;
                let mut last = old.last_practiced;
                if !was_done && lessons >= lesson_count {
                    reps = reps.max(1);
                } else if was_done {
                    reps += 1;
                }
                if lessons >= lesson_count {
                    last = last.max(at);
                }
                let (accuracy, answers) = blend(old.accuracy, old.answers, correct, total);
                db::write_skill(tx, user_id, course_id, skill, &SkillProgress { lessons, last_practiced: last, reps, accuracy, answers })?;
            }
            "practice" => {
                for s in e["skills"].as_array().unwrap() {
                    let skill = s.as_str().unwrap();
                    if let Some(p) = db::read_skill(tx, user_id, course_id, skill)? {
                        let (accuracy, answers) = blend(p.accuracy, p.answers, correct, total);
                        let next = SkillProgress { last_practiced: p.last_practiced.max(at), reps: p.reps + 1, accuracy, answers, ..p };
                        db::write_skill(tx, user_id, course_id, skill, &next)?;
                    }
                }
            }
            "reset" => {
                db::reset_course(tx, user_id, course_id)?;
                return Ok(()); // no XP for a reset
            }
            _ => unreachable!("validated"),
        }
        let xp = int(e, "xp").unwrap();
        if xp > 0 {
            db::add_xp(tx, user_id, string(e, "day").unwrap(), xp)?;
        }
        Ok(())
    }

    /// Full progress document returned to the app.
    pub fn snapshot(&self, user_id: i64) -> DbResult<Value> {
        let mut courses = Map::new();
        for (course, skills) in self.db.progress(user_id)? {
            let mut out = Map::new();
            for (skill, p) in skills {
                out.insert(
                    skill,
                    json!({
                        "lessons": p.lessons,
                        "lastPracticed": p.last_practiced,
                        "reps": p.reps,
                        "accuracy": (p.accuracy * 1000.0).round() / 1000.0,
                        "answers": p.answers,
                    }),
                );
            }
            courses.insert(course, Value::Object(out));
        }
        Ok(json!({ "courses": courses, "xp": self.db.xp(user_id)?, "serverTime": db::now_ms() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accuracy_blending_matches_app() {
        let (a, n) = blend(0.0, 0, 8, 10);
        assert!((a - 0.8).abs() < 1e-9);
        assert_eq!(n, 10);
        let (a, n) = blend(0.8, 10, 10, 10);
        assert!((a - (0.8 * 0.65 + 0.35)).abs() < 1e-9);
        assert_eq!(n, 20);
        assert_eq!(blend(0.5, 4, 0, 0), (0.5, 4));
    }

    #[test]
    fn event_ids() {
        assert!(event_id_ok("evt-000001"));
        assert!(event_id_ok("6f1c1a3e-6d1f-4a0e-9a54-6a5d1f0c2b11"));
        assert!(!event_id_ok("short"));
        assert!(!event_id_ok("has space in it"));
    }
}
