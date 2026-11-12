# Parla Server

Backend for [Parla](https://github.com/Parla-Studio/parla), a calm, simple language learning app for Android:
accounts, course delivery and progress sync. Written in Rust (axum, SQLite via rusqlite with bundled SQLite,
Argon2id passwords). Ships as a single binary and as a Debian package.

```
parla-server/
├── content/courses/   Course files (en.json, fr.json) – the single source of lesson content
├── src/               Server code (api, auth, content, db, progress)
├── tests/             End-to-end tests against a real SQLite database
├── packaging/         Debian package (build-deb.sh, systemd unit, default config)
├── deploy/            nginx example
├── scripts/           setup-rust.sh for build machines
└── Jenkinsfile        Tests, builds the binary and the .deb, optionally publishes course files
```

## How it fits together

* **Content comes from the server.** The app ships without any lessons. On every start and whenever it returns to
  the foreground it asks the server for the course list hash; only courses whose hash changed are downloaded and
  cached for offline use. Editing a file in the server's content folder is enough – no app update, no restart.
* **Accounts.** Username + password (Argon2id), random bearer tokens that are stored only as SHA-256 hashes,
  rate-limited login and registration, password change (signs out other devices) and account deletion.
* **Progress sync.** Finishing a lesson creates an event with a random id. The app applies events locally right
  away (works offline) and uploads them in the background. The server applies each id at most once, so retries
  are safe, and returns the authoritative progress snapshot.
* **Adaptive difficulty.** Sessions report how many answers were right. The server keeps a running accuracy per
  skill (exponential moving average, newest session weighted 35 %); the app turns it into the exercise mix.

## Build and run

Requirements: a Rust toolchain and a C compiler. `scripts/setup-rust.sh` installs Rust via rustup if `cargo`
is missing.

```bash
cargo build --release
./target/release/parla-server                 # reads ./parla.toml
```

The course files are built into the binary. On first start the server writes a commented `parla.toml`,
creates the SQLite database (`data/parla.db`) and, if the content folder doesn't exist yet, fills it with the
built-in courses. Run from the repository root, it uses `content/` directly. Defaults: listens on
`127.0.0.1:8085`, registration open, sessions expire after 180 days without use. `RUST_LOG=debug` gives more
log output.

## Debian package

```bash
packaging/build-deb.sh          # -> target/debian/parla-server_<version>_<arch>.deb
sudo apt install ./target/debian/parla-server_*.deb
```

Jenkins builds the package on every run. It installs:

| Path | What |
|---|---|
| `/usr/bin/parla-server` | the server (courses built in) |
| `/etc/parla/parla.toml` | configuration (kept on upgrades) |
| `/var/lib/parla/` | database and `content/` course files – back this up |
| `parla.service` | systemd service, enabled and started on install, restarted on upgrade |

The service runs as the system user `parla` on `127.0.0.1:8085`. Logs: `journalctl -u parla`.
`apt remove` keeps data and config; `apt purge` deletes both and the user. The binary needs glibc 2.34+
(Debian 12, Ubuntu 22.04 or newer).

Put a TLS reverse proxy in front of it – see [`deploy/nginx-parla.conf`](deploy/nginx-parla.conf)
(also installed to `/usr/share/doc/parla-server/examples/`).

## API

Bodies are JSON; authenticated endpoints expect `Authorization: Bearer <token>`. Errors are
`{"error": code, "message": German text}`.

| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/api/health` | – | Version check |
| POST | `/api/auth/register` | – | `{username, password, device?}` → `{token, user}` |
| POST | `/api/auth/login` | – | `{username, password, device?}` → `{token, user}` |
| POST | `/api/auth/logout` | ✓ | Ends this session |
| POST | `/api/auth/password` | ✓ | `{oldPassword, newPassword}`; signs out other devices |
| DELETE | `/api/account` | ✓ | `{password}`; deletes the account and all progress |
| GET | `/api/me` | ✓ | User and settings |
| PUT | `/api/settings` | ✓ | `{goal?, sound?, listen?, course?}` |
| GET | `/api/courses` | – | `{hash, courses: [{id, name, flag, version, hash, skills}]}`, supports `If-None-Match` |
| GET | `/api/courses/{id}` | – | Course document, `ETag` = content hash |
| GET | `/api/progress` | ✓ | `{courses: {course: {skill: {lessons, lastPracticed, reps, accuracy, answers}}}, xp: {day: xp}}` |
| POST | `/api/progress/events` | ✓ | `{events: [...]}` → `{applied, duplicates, rejected, progress}` |

Event types: `lesson` (`skill`, `lesson`, `xp`, `day`, `at`), `practice` (`skills`, `xp`, `day`, `at`) and
`reset` (clears one course). Lessons and practice sessions may carry `correct` and `total` (graded answers,
at most 100), which update the per-skill `accuracy` (0..1) and `answers`. XP per event is capped at 30, at most
200 events per request.

## Adding or changing content

Course files live in the server's content folder (`content_dir`, `/var/lib/parla/content` for the package). The
server checks for changes every few seconds; an invalid file is logged and ignored while the previous version
stays online, and a deleted file removes the course. The built-in courses are only copied into an *empty*
content folder, so a package upgrade never overwrites your course files – copy new `content/courses/*.json` to
the server, or let the Jenkins "Publish content" stage do it (set `PARLA_CONTENT_TARGET`).
See [`content/README.md`](content/README.md) for the format.

## Tests

`cargo test` – unit tests plus end-to-end tests against the real router with a temporary SQLite database and
content folder: accounts, closed registration, rate limiting, ETags, hot reload of course files (including broken
and deleted files), idempotent events with accuracy tracking, settings, password change and account deletion.
