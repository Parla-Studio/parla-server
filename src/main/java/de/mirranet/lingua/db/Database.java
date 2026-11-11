package de.mirranet.lingua.db;

import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.TreeMap;

/**
 * SQLite storage. A single connection is shared and guarded by this object's monitor, which is plenty
 * for the load of a personal language app and keeps SQLite's locking simple.
 */
public final class Database implements AutoCloseable {

    private static final int SCHEMA_VERSION = 2;

    public record User(long id, String username, String passwordHash, long createdAt) {
    }

    /**
     * @param accuracy share of correct answers (0..1), weighted towards recent sessions
     * @param answers  total number of graded answers seen for this skill
     */
    public record SkillProgress(int lessons, long lastPracticed, int reps, double accuracy, int answers) {
        public SkillProgress(int lessons, long lastPracticed, int reps) {
            this(lessons, lastPracticed, reps, 0, 0);
        }
    }

    @FunctionalInterface
    public interface Work<T> {
        T run(Connection c) throws SQLException;
    }

    private final Connection conn;

    public Database(Path file) throws SQLException {
        try {
            Path parent = file.toAbsolutePath().getParent();
            if (parent != null) {
                Files.createDirectories(parent);
            }
        } catch (java.io.IOException e) {
            throw new SQLException("Cannot create database directory", e);
        }
        conn = DriverManager.getConnection("jdbc:sqlite:" + file);
        try (Statement st = conn.createStatement()) {
            st.execute("PRAGMA journal_mode=WAL");
            st.execute("PRAGMA foreign_keys=ON");
            st.execute("PRAGMA busy_timeout=5000");
        }
        migrate();
    }

    private void migrate() throws SQLException {
        int current;
        try (Statement st = conn.createStatement(); ResultSet rs = st.executeQuery("PRAGMA user_version")) {
            current = rs.next() ? rs.getInt(1) : 0;
        }
        if (current >= SCHEMA_VERSION) {
            return;
        }
        if (current < 1) {
            createInitialSchema();
        }
        if (current < 2) {
            // v2: per-skill accuracy, used by the app to adapt exercise difficulty.
            transaction(c -> {
                try (Statement st = c.createStatement()) {
                    st.execute("ALTER TABLE skill_progress ADD COLUMN accuracy REAL NOT NULL DEFAULT 0");
                    st.execute("ALTER TABLE skill_progress ADD COLUMN answers INTEGER NOT NULL DEFAULT 0");
                    st.execute("PRAGMA user_version = 2");
                }
                return null;
            });
        }
    }

    private void createInitialSchema() throws SQLException {
        transaction(c -> {
            try (Statement st = c.createStatement()) {
                st.execute("""
                        CREATE TABLE IF NOT EXISTS users (
                          id INTEGER PRIMARY KEY AUTOINCREMENT,
                          username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                          password_hash TEXT NOT NULL,
                          created_at INTEGER NOT NULL)""");
                st.execute("""
                        CREATE TABLE IF NOT EXISTS sessions (
                          token_hash TEXT PRIMARY KEY,
                          user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                          device TEXT,
                          created_at INTEGER NOT NULL,
                          last_used INTEGER NOT NULL)""");
                st.execute("CREATE INDEX IF NOT EXISTS sessions_user ON sessions(user_id)");
                st.execute("""
                        CREATE TABLE IF NOT EXISTS skill_progress (
                          user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                          course TEXT NOT NULL,
                          skill TEXT NOT NULL,
                          lessons INTEGER NOT NULL DEFAULT 0,
                          last_practiced INTEGER NOT NULL DEFAULT 0,
                          reps INTEGER NOT NULL DEFAULT 0,
                          PRIMARY KEY (user_id, course, skill))""");
                st.execute("""
                        CREATE TABLE IF NOT EXISTS xp_log (
                          user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                          day TEXT NOT NULL,
                          xp INTEGER NOT NULL,
                          PRIMARY KEY (user_id, day))""");
                st.execute("""
                        CREATE TABLE IF NOT EXISTS applied_events (
                          user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                          event_id TEXT NOT NULL,
                          applied_at INTEGER NOT NULL,
                          PRIMARY KEY (user_id, event_id))""");
                st.execute("""
                        CREATE TABLE IF NOT EXISTS settings (
                          user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                          json TEXT NOT NULL)""");
                st.execute("PRAGMA user_version = 1");
            }
            return null;
        });
    }

    /** Runs work inside a transaction; rolls back on any exception. */
    public synchronized <T> T transaction(Work<T> work) throws SQLException {
        boolean auto = conn.getAutoCommit();
        conn.setAutoCommit(false);
        try {
            T result = work.run(conn);
            conn.commit();
            return result;
        } catch (SQLException | RuntimeException e) {
            conn.rollback();
            throw e;
        } finally {
            conn.setAutoCommit(auto);
        }
    }

    // ------------------------------------------------------------------ users

    /** @return the new user id, or -1 if the username is taken. */
    public synchronized long createUser(String username, String passwordHash) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO users(username, password_hash, created_at) VALUES (?,?,?) ON CONFLICT(username) DO NOTHING",
                Statement.RETURN_GENERATED_KEYS)) {
            ps.setString(1, username);
            ps.setString(2, passwordHash);
            ps.setLong(3, System.currentTimeMillis());
            if (ps.executeUpdate() == 0) {
                return -1;
            }
            try (ResultSet rs = ps.getGeneratedKeys()) {
                return rs.next() ? rs.getLong(1) : -1;
            }
        }
    }

    public synchronized User findUser(String username) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement(
                "SELECT id, username, password_hash, created_at FROM users WHERE username = ?")) {
            ps.setString(1, username);
            return readUser(ps);
        }
    }

    public synchronized User userById(long id) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement(
                "SELECT id, username, password_hash, created_at FROM users WHERE id = ?")) {
            ps.setLong(1, id);
            return readUser(ps);
        }
    }

    private static User readUser(PreparedStatement ps) throws SQLException {
        try (ResultSet rs = ps.executeQuery()) {
            return rs.next() ? new User(rs.getLong(1), rs.getString(2), rs.getString(3), rs.getLong(4)) : null;
        }
    }

    public synchronized void updatePassword(long userId, String passwordHash) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("UPDATE users SET password_hash = ? WHERE id = ?")) {
            ps.setString(1, passwordHash);
            ps.setLong(2, userId);
            ps.executeUpdate();
        }
    }

    public synchronized void deleteUser(long userId) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("DELETE FROM users WHERE id = ?")) {
            ps.setLong(1, userId);
            ps.executeUpdate();
        }
    }

    // ------------------------------------------------------------------ sessions

    public synchronized void createSession(long userId, String tokenHash, String device) throws SQLException {
        long now = System.currentTimeMillis();
        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO sessions(token_hash, user_id, device, created_at, last_used) VALUES (?,?,?,?,?)")) {
            ps.setString(1, tokenHash);
            ps.setLong(2, userId);
            ps.setString(3, device);
            ps.setLong(4, now);
            ps.setLong(5, now);
            ps.executeUpdate();
        }
    }

    /**
     * Looks up a session and slides its expiry. Sessions unused for longer than {@code maxIdleMillis} are deleted.
     *
     * @return the user id, or -1 if the token is unknown or expired.
     */
    public synchronized long sessionUser(String tokenHash, long maxIdleMillis) throws SQLException {
        long now = System.currentTimeMillis();
        long userId = -1, lastUsed = 0;
        try (PreparedStatement ps = conn.prepareStatement("SELECT user_id, last_used FROM sessions WHERE token_hash = ?")) {
            ps.setString(1, tokenHash);
            try (ResultSet rs = ps.executeQuery()) {
                if (rs.next()) {
                    userId = rs.getLong(1);
                    lastUsed = rs.getLong(2);
                }
            }
        }
        if (userId < 0) {
            return -1;
        }
        if (now - lastUsed > maxIdleMillis) {
            deleteSession(tokenHash);
            return -1;
        }
        // Only write when it matters, to keep reads cheap.
        if (now - lastUsed > 60_000) {
            try (PreparedStatement ps = conn.prepareStatement("UPDATE sessions SET last_used = ? WHERE token_hash = ?")) {
                ps.setLong(1, now);
                ps.setString(2, tokenHash);
                ps.executeUpdate();
            }
        }
        return userId;
    }

    public synchronized void deleteSession(String tokenHash) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("DELETE FROM sessions WHERE token_hash = ?")) {
            ps.setString(1, tokenHash);
            ps.executeUpdate();
        }
    }

    public synchronized void deleteOtherSessions(long userId, String keepTokenHash) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("DELETE FROM sessions WHERE user_id = ? AND token_hash <> ?")) {
            ps.setLong(1, userId);
            ps.setString(2, keepTokenHash);
            ps.executeUpdate();
        }
    }

    public synchronized int purgeIdleSessions(long maxIdleMillis) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("DELETE FROM sessions WHERE last_used < ?")) {
            ps.setLong(1, System.currentTimeMillis() - maxIdleMillis);
            return ps.executeUpdate();
        }
    }

    // ------------------------------------------------------------------ progress

    /** course -> skill -> progress */
    public synchronized Map<String, Map<String, SkillProgress>> progress(long userId) throws SQLException {
        Map<String, Map<String, SkillProgress>> out = new TreeMap<>();
        try (PreparedStatement ps = conn.prepareStatement(
                "SELECT course, skill, lessons, last_practiced, reps, accuracy, answers FROM skill_progress WHERE user_id = ?")) {
            ps.setLong(1, userId);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    out.computeIfAbsent(rs.getString(1), k -> new LinkedHashMap<>())
                            .put(rs.getString(2), new SkillProgress(rs.getInt(3), rs.getLong(4), rs.getInt(5), rs.getDouble(6), rs.getInt(7)));
                }
            }
        }
        return out;
    }

    /** day (yyyy-MM-dd) -> xp */
    public synchronized Map<String, Long> xp(long userId) throws SQLException {
        Map<String, Long> out = new TreeMap<>();
        try (PreparedStatement ps = conn.prepareStatement("SELECT day, xp FROM xp_log WHERE user_id = ?")) {
            ps.setLong(1, userId);
            try (ResultSet rs = ps.executeQuery()) {
                while (rs.next()) {
                    out.put(rs.getString(1), rs.getLong(2));
                }
            }
        }
        return out;
    }

    /** Must be called inside {@link #transaction}. @return false if the event was already applied. */
    public static boolean markEvent(Connection c, long userId, String eventId) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "INSERT INTO applied_events(user_id, event_id, applied_at) VALUES (?,?,?) ON CONFLICT DO NOTHING")) {
            ps.setLong(1, userId);
            ps.setString(2, eventId);
            ps.setLong(3, System.currentTimeMillis());
            return ps.executeUpdate() > 0;
        }
    }

    public static SkillProgress readSkill(Connection c, long userId, String course, String skill) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(
                "SELECT lessons, last_practiced, reps, accuracy, answers FROM skill_progress WHERE user_id = ? AND course = ? AND skill = ?")) {
            ps.setLong(1, userId);
            ps.setString(2, course);
            ps.setString(3, skill);
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? new SkillProgress(rs.getInt(1), rs.getLong(2), rs.getInt(3), rs.getDouble(4), rs.getInt(5)) : null;
            }
        }
    }

    public static void writeSkill(Connection c, long userId, String course, String skill, SkillProgress p) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("""
                INSERT INTO skill_progress(user_id, course, skill, lessons, last_practiced, reps, accuracy, answers)
                VALUES (?,?,?,?,?,?,?,?)
                ON CONFLICT(user_id, course, skill) DO UPDATE SET
                  lessons = excluded.lessons, last_practiced = excluded.last_practiced, reps = excluded.reps,
                  accuracy = excluded.accuracy, answers = excluded.answers""")) {
            ps.setLong(1, userId);
            ps.setString(2, course);
            ps.setString(3, skill);
            ps.setInt(4, p.lessons());
            ps.setLong(5, p.lastPracticed());
            ps.setInt(6, p.reps());
            ps.setDouble(7, p.accuracy());
            ps.setInt(8, p.answers());
            ps.executeUpdate();
        }
    }

    public static void addXp(Connection c, long userId, String day, long xp) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement("""
                INSERT INTO xp_log(user_id, day, xp) VALUES (?,?,?)
                ON CONFLICT(user_id, day) DO UPDATE SET xp = xp + excluded.xp""")) {
            ps.setLong(1, userId);
            ps.setString(2, day);
            ps.setLong(3, xp);
            ps.executeUpdate();
        }
    }

    public synchronized void resetCourse(long userId, String course) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("DELETE FROM skill_progress WHERE user_id = ? AND course = ?")) {
            ps.setLong(1, userId);
            ps.setString(2, course);
            ps.executeUpdate();
        }
    }

    public synchronized int purgeOldEvents(long olderThanMillis) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("DELETE FROM applied_events WHERE applied_at < ?")) {
            ps.setLong(1, System.currentTimeMillis() - olderThanMillis);
            return ps.executeUpdate();
        }
    }

    // ------------------------------------------------------------------ settings

    public synchronized String settings(long userId) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement("SELECT json FROM settings WHERE user_id = ?")) {
            ps.setLong(1, userId);
            try (ResultSet rs = ps.executeQuery()) {
                return rs.next() ? rs.getString(1) : null;
            }
        }
    }

    public synchronized void saveSettings(long userId, String json) throws SQLException {
        try (PreparedStatement ps = conn.prepareStatement(
                "INSERT INTO settings(user_id, json) VALUES (?,?) ON CONFLICT(user_id) DO UPDATE SET json = excluded.json")) {
            ps.setLong(1, userId);
            ps.setString(2, json);
            ps.executeUpdate();
        }
    }

    @Override
    public synchronized void close() throws SQLException {
        conn.close();
    }
}
