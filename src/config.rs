//! Server configuration, read from a TOML file (default `parla.toml` in the working directory).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, path::Path, path::PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Address to listen on. Keep 127.0.0.1 behind a reverse proxy (recommended, for TLS).
    pub bind: String,
    pub port: u16,
    /// SQLite database file (created automatically).
    pub database: PathBuf,
    /// Folder with course files (`<id>.json`); changes are picked up without a restart.
    pub content_dir: PathBuf,
    /// Allow new accounts to be created from the app.
    pub registration_open: bool,
    /// How long a login stays valid without being used.
    pub session_days: u32,
    /// Use the X-Forwarded-For header for the client address (only behind your own reverse proxy).
    pub trust_proxy: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 8085,
            database: "data/parla.db".into(),
            content_dir: "content".into(),
            registration_open: true,
            session_days: 180,
            trust_proxy: true,
        }
    }
}

pub const DEFAULT_FILE: &str = r#"# Parla server configuration

# Address and port to listen on. Keep 127.0.0.1 when running behind a reverse proxy (recommended, for TLS).
bind = "127.0.0.1"
port = 8085

# SQLite database file (created automatically).
database = "data/parla.db"

# Folder with course files (<id>.json). Changes are picked up automatically without a restart.
# If the folder does not exist, it is created and filled with the courses built into the server.
content_dir = "content"

# Allow new accounts to be created from the app.
registration_open = true

# How long a login stays valid without being used (days).
session_days = 180

# Use the X-Forwarded-For header for the client address (only when behind your own reverse proxy).
trust_proxy = true
"#;

impl Config {
    /// Loads the file, writing the commented default first if it does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            fs::write(path, DEFAULT_FILE).with_context(|| format!("cannot write default config to {}", path.display()))?;
        }
        let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))
    }

    pub fn session_max_idle_ms(&self) -> i64 {
        i64::from(self.session_days) * 86_400_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_defaults() {
        let parsed: Config = toml::from_str(DEFAULT_FILE).unwrap();
        let def = Config::default();
        assert_eq!(parsed.port, def.port);
        assert_eq!(parsed.database, def.database);
        assert_eq!(parsed.content_dir, def.content_dir);
        assert_eq!(parsed.session_days, def.session_days);
    }
}
