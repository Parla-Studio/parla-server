//! MirraLingua server: accounts, course delivery with hot reload, and idempotent progress sync
//! for the MirraLingua Android app.

pub mod api;
pub mod auth;
pub mod config;
pub mod content;
pub mod db;
pub mod progress;

use std::sync::Arc;
use std::time::Duration;

/// How often the content directory is checked for changed course files.
pub const CONTENT_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Opens database and content and builds the shared application state.
pub fn build_state(config: config::Config, content_check_interval: Duration) -> anyhow::Result<api::AppState> {
    let db = Arc::new(db::Db::open(&config.database)?);
    let courses = Arc::new(content::CourseRepository::open(&config.content_dir, content_check_interval)?);
    Ok(api::AppState::new(config, db, courses))
}
