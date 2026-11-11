package de.mirranet.lingua.api;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpHandler;
import de.mirranet.lingua.Config;
import de.mirranet.lingua.auth.PasswordHasher;
import de.mirranet.lingua.auth.RateLimiter;
import de.mirranet.lingua.auth.Tokens;
import de.mirranet.lingua.content.CourseRepository;
import de.mirranet.lingua.content.CourseRepository.Course;
import de.mirranet.lingua.db.Database;
import de.mirranet.lingua.progress.ProgressService;
import de.mirranet.lingua.util.Json;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.regex.Pattern;

/**
 * HTTP API used by the Android app. All bodies are JSON; authenticated endpoints expect
 * {@code Authorization: Bearer <token>}.
 *
 * <pre>
 * GET    /api/health
 * POST   /api/auth/register      {username, password, device?}      -> {token, user}
 * POST   /api/auth/login         {username, password, device?}      -> {token, user}
 * POST   /api/auth/logout                                           -> 204
 * POST   /api/auth/password      {oldPassword, newPassword}         -> 204 (signs out other devices)
 * DELETE /api/account            {password}                         -> 204
 * GET    /api/me                                                    -> {user, settings}
 * PUT    /api/settings           {goal?, sound?, listen?, course?}  -> settings
 * GET    /api/courses                                               -> {hash, courses:[{id,name,flag,version,hash}]}
 * GET    /api/courses/{id}                                          -> course document (ETag = hash)
 * GET    /api/progress                                              -> {courses, xp, serverTime}
 *        (per skill: lessons, lastPracticed, reps, accuracy 0..1, answers)
 * POST   /api/progress/events    {events:[...]}                     -> {applied, duplicates, rejected, progress}
 * </pre>
 */
public final class Api implements HttpHandler {

    private static final Logger LOG = Logger.getLogger(Api.class.getName());
    private static final int MAX_BODY = 256 * 1024;
    private static final Pattern USERNAME = Pattern.compile("[A-Za-z0-9._-]{3,32}");
    private static final Pattern COURSE_PATH = Pattern.compile("/api/courses/([a-z0-9_-]{1,32})");
    private static final List<Long> GOALS = List.of(10L, 20L, 30L, 50L);

    private final Config config;
    private final Database db;
    private final CourseRepository courses;
    private final ProgressService progress;
    private final String version;
    private final RateLimiter loginLimiter = new RateLimiter(8, 15 * 60_000L);
    private final RateLimiter registerLimiter = new RateLimiter(10, 60 * 60_000L);

    /** Signals an HTTP error with a JSON body {error, message}. */
    static final class ApiError extends RuntimeException {
        final int status;
        final String code;

        ApiError(int status, String code, String message) {
            super(message);
            this.status = status;
            this.code = code;
        }
    }

    private record Auth(long userId, String tokenHash) {
    }

    public Api(Config config, Database db, CourseRepository courses, ProgressService progress, String version) {
        this.config = config;
        this.db = db;
        this.courses = courses;
        this.progress = progress;
        this.version = version;
    }

    @Override
    public void handle(HttpExchange ex) throws IOException {
        try (ex) {
            try {
                route(ex);
            } catch (ApiError e) {
                send(ex, e.status, Map.of("error", e.code, "message", e.getMessage()));
            } catch (Json.JsonException e) {
                send(ex, 400, Map.of("error", "bad_json", "message", "Ungültige Anfrage."));
            } catch (SQLException e) {
                LOG.log(Level.SEVERE, "Database error on " + ex.getRequestURI(), e);
                send(ex, 500, Map.of("error", "server", "message", "Serverfehler. Bitte später erneut versuchen."));
            } catch (RuntimeException e) {
                LOG.log(Level.SEVERE, "Unexpected error on " + ex.getRequestURI(), e);
                send(ex, 500, Map.of("error", "server", "message", "Serverfehler. Bitte später erneut versuchen."));
            }
        }
    }

    private void route(HttpExchange ex) throws IOException, SQLException {
        String method = ex.getRequestMethod();
        String path = ex.getRequestURI().getPath();
        if (path.length() > 1 && path.endsWith("/")) {
            path = path.substring(0, path.length() - 1);
        }

        switch (method + " " + path) {
            case "GET /api/health" -> send(ex, 200, Map.of("ok", true, "version", version));
            case "POST /api/auth/register" -> register(ex);
            case "POST /api/auth/login" -> login(ex);
            case "POST /api/auth/logout" -> {
                Auth a = auth(ex);
                db.deleteSession(a.tokenHash());
                sendEmpty(ex, 204);
            }
            case "POST /api/auth/password" -> changePassword(ex);
            case "DELETE /api/account" -> deleteAccount(ex);
            case "GET /api/me" -> {
                Auth a = auth(ex);
                Database.User u = db.userById(a.userId());
                send(ex, 200, Map.of("user", userJson(u), "settings", settings(a.userId())));
            }
            case "PUT /api/settings" -> updateSettings(ex);
            case "GET /api/courses" -> listCourses(ex);
            case "GET /api/progress" -> send(ex, 200, progress.snapshot(auth(ex).userId()));
            case "POST /api/progress/events" -> postEvents(ex);
            default -> {
                var m = COURSE_PATH.matcher(path);
                if (method.equals("GET") && m.matches()) {
                    courseDocument(ex, m.group(1));
                } else if (path.startsWith("/api/")) {
                    throw new ApiError(404, "not_found", "Unbekannter Endpunkt.");
                } else {
                    sendText(ex, 200, "text/plain", "MirraLingua server " + version + "\n");
                }
            }
        }
    }

    // ------------------------------------------------------------------ auth

    private void register(HttpExchange ex) throws IOException, SQLException {
        if (!config.registrationOpen()) {
            throw new ApiError(403, "registration_closed", "Die Registrierung ist auf diesem Server deaktiviert.");
        }
        String ip = clientIp(ex);
        if (registerLimiter.isBlocked(ip)) {
            throw new ApiError(429, "rate_limited", "Zu viele Registrierungen. Bitte später erneut versuchen.");
        }
        Map<String, Object> body = body(ex);
        String username = trimmed(Json.str(body, "username"));
        String password = Json.str(body, "password");
        if (username == null || !USERNAME.matcher(username).matches()) {
            throw new ApiError(400, "invalid_username", "Benutzername: 3–32 Zeichen, nur Buchstaben, Ziffern, Punkt, Minus und Unterstrich.");
        }
        checkPassword(password);
        long id = db.createUser(username, PasswordHasher.hash(password));
        if (id < 0) {
            throw new ApiError(409, "username_taken", "Dieser Benutzername ist schon vergeben.");
        }
        registerLimiter.recordFailure(ip); // counts registrations per address
        LOG.info("New account '" + username + "' from " + ip);
        send(ex, 201, newSession(db.userById(id), Json.str(body, "device")));
    }

    private void login(HttpExchange ex) throws IOException, SQLException {
        Map<String, Object> body = body(ex);
        String username = trimmed(Json.str(body, "username"));
        String password = Json.str(body, "password");
        if (username == null || password == null) {
            throw new ApiError(400, "missing_fields", "Benutzername und Passwort angeben.");
        }
        String key = clientIp(ex) + "|" + username.toLowerCase();
        if (loginLimiter.isBlocked(key)) {
            throw new ApiError(429, "rate_limited", "Zu viele Fehlversuche. Bitte in 15 Minuten erneut versuchen.");
        }
        Database.User u = db.findUser(username);
        boolean ok = PasswordHasher.verify(password, u == null ? PasswordHasher.DUMMY_HASH : u.passwordHash()) && u != null;
        if (!ok) {
            loginLimiter.recordFailure(key);
            throw new ApiError(401, "invalid_credentials", "Benutzername oder Passwort ist falsch.");
        }
        loginLimiter.reset(key);
        send(ex, 200, newSession(u, Json.str(body, "device")));
    }

    private Map<String, Object> newSession(Database.User u, String device) throws SQLException {
        String token = Tokens.newToken();
        String dev = device == null ? null : device.substring(0, Math.min(device.length(), 100));
        db.createSession(u.id(), Tokens.sha256(token), dev);
        return Map.of("token", token, "user", userJson(u));
    }

    private void changePassword(HttpExchange ex) throws IOException, SQLException {
        Auth a = auth(ex);
        Map<String, Object> body = body(ex);
        Database.User u = db.userById(a.userId());
        if (!PasswordHasher.verify(String.valueOf(Json.str(body, "oldPassword")), u.passwordHash())) {
            throw new ApiError(403, "wrong_password", "Das aktuelle Passwort ist falsch.");
        }
        String next = Json.str(body, "newPassword");
        checkPassword(next);
        db.updatePassword(u.id(), PasswordHasher.hash(next));
        db.deleteOtherSessions(u.id(), a.tokenHash());
        sendEmpty(ex, 204);
    }

    private void deleteAccount(HttpExchange ex) throws IOException, SQLException {
        Auth a = auth(ex);
        Map<String, Object> body = body(ex);
        Database.User u = db.userById(a.userId());
        if (!PasswordHasher.verify(String.valueOf(Json.str(body, "password")), u.passwordHash())) {
            throw new ApiError(403, "wrong_password", "Das Passwort ist falsch.");
        }
        db.deleteUser(u.id());
        LOG.info("Deleted account '" + u.username() + "'");
        sendEmpty(ex, 204);
    }

    private static void checkPassword(String password) {
        if (password == null || password.length() < 8 || password.length() > 200) {
            throw new ApiError(400, "weak_password", "Das Passwort muss mindestens 8 Zeichen lang sein.");
        }
    }

    private Auth auth(HttpExchange ex) throws SQLException {
        String header = ex.getRequestHeaders().getFirst("Authorization");
        if (header == null || !header.startsWith("Bearer ") || header.length() < 20) {
            throw new ApiError(401, "unauthorized", "Bitte anmelden.");
        }
        String hash = Tokens.sha256(header.substring(7).trim());
        long userId = db.sessionUser(hash, config.sessionDays() * 86_400_000L);
        if (userId < 0) {
            throw new ApiError(401, "unauthorized", "Deine Sitzung ist abgelaufen. Bitte erneut anmelden.");
        }
        return new Auth(userId, hash);
    }

    private static Map<String, Object> userJson(Database.User u) {
        return Map.of("id", u.id(), "username", u.username(), "createdAt", u.createdAt());
    }

    // ------------------------------------------------------------------ settings

    private Map<String, Object> settings(long userId) throws SQLException {
        Map<String, Object> s = new LinkedHashMap<>(Map.of("goal", 20L, "sound", true, "listen", true));
        String stored = db.settings(userId);
        if (stored != null) {
            s.putAll(Json.parseObject(stored));
        }
        return s;
    }

    private void updateSettings(HttpExchange ex) throws IOException, SQLException {
        Auth a = auth(ex);
        Map<String, Object> body = body(ex);
        Map<String, Object> s = settings(a.userId());
        Long goal = Json.num(body, "goal");
        if (goal != null) {
            if (!GOALS.contains(goal)) {
                throw new ApiError(400, "invalid_goal", "Tagesziel muss 10, 20, 30 oder 50 sein.");
            }
            s.put("goal", goal);
        }
        for (String key : List.of("sound", "listen")) {
            Boolean b = Json.bool(body, key);
            if (b != null) {
                s.put(key, b);
            }
        }
        if (body.containsKey("course")) {
            String course = Json.str(body, "course");
            if (course != null && courses.get(course) == null) {
                throw new ApiError(400, "unknown_course", "Unbekannter Kurs.");
            }
            s.put("course", course);
        }
        db.saveSettings(a.userId(), Json.write(s));
        send(ex, 200, s);
    }

    // ------------------------------------------------------------------ content

    private void listCourses(HttpExchange ex) throws IOException {
        List<Map<String, Object>> list = new ArrayList<>();
        StringBuilder all = new StringBuilder();
        for (Course c : courses.list()) {
            Map<String, Object> m = new LinkedHashMap<>();
            m.put("id", c.id());
            m.put("name", c.name());
            m.put("flag", c.flag());
            m.put("version", c.version());
            m.put("hash", c.hash());
            m.put("skills", c.lessonsPerSkill().size());
            list.add(m);
            all.append(c.id()).append(':').append(c.hash()).append(';');
        }
        String hash = Tokens.sha256(all.toString()).substring(0, 16);
        if (notModified(ex, hash)) {
            return;
        }
        ex.getResponseHeaders().set("ETag", "\"" + hash + "\"");
        send(ex, 200, Map.of("hash", hash, "courses", list));
    }

    private void courseDocument(HttpExchange ex, String id) throws IOException {
        Course c = courses.get(id);
        if (c == null) {
            throw new ApiError(404, "unknown_course", "Unbekannter Kurs.");
        }
        if (notModified(ex, c.hash())) {
            return;
        }
        ex.getResponseHeaders().set("ETag", "\"" + c.hash() + "\"");
        ex.getResponseHeaders().set("X-Course-Hash", c.hash());
        sendText(ex, 200, "application/json; charset=utf-8", c.json());
    }

    private static boolean notModified(HttpExchange ex, String hash) throws IOException {
        String inm = ex.getRequestHeaders().getFirst("If-None-Match");
        if (inm != null && inm.replace("\"", "").trim().equals(hash)) {
            ex.getResponseHeaders().set("ETag", "\"" + hash + "\"");
            ex.sendResponseHeaders(304, -1);
            return true;
        }
        return false;
    }

    // ------------------------------------------------------------------ progress

    private void postEvents(HttpExchange ex) throws IOException, SQLException {
        Auth a = auth(ex);
        Map<String, Object> body = body(ex);
        List<Object> events = Json.arr(body, "events");
        if (events == null) {
            throw new ApiError(400, "missing_events", "Feld 'events' fehlt.");
        }
        if (events.size() > ProgressService.MAX_EVENTS_PER_REQUEST) {
            throw new ApiError(413, "too_many_events", "Höchstens " + ProgressService.MAX_EVENTS_PER_REQUEST + " Ereignisse pro Anfrage.");
        }
        ProgressService.Result r = progress.apply(a.userId(), events);
        if (!r.rejected().isEmpty()) {
            LOG.warning("User " + a.userId() + ": rejected " + r.rejected().size() + " event(s): " + r.rejected());
        }
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("applied", r.applied());
        out.put("duplicates", r.duplicates());
        out.put("rejected", r.rejected());
        out.put("progress", progress.snapshot(a.userId()));
        send(ex, 200, out);
    }

    // ------------------------------------------------------------------ helpers

    private String clientIp(HttpExchange ex) {
        if (config.trustProxy()) {
            String xff = ex.getRequestHeaders().getFirst("X-Forwarded-For");
            if (xff != null && !xff.isBlank()) {
                return xff.split(",")[0].trim();
            }
        }
        return ex.getRemoteAddress().getAddress().getHostAddress();
    }

    private static String trimmed(String s) {
        return s == null ? null : s.trim();
    }

    private static Map<String, Object> body(HttpExchange ex) throws IOException {
        try (InputStream in = ex.getRequestBody()) {
            ByteArrayOutputStream buf = new ByteArrayOutputStream();
            byte[] chunk = new byte[8192];
            int n;
            while ((n = in.read(chunk)) > 0) {
                buf.write(chunk, 0, n);
                if (buf.size() > MAX_BODY) {
                    throw new ApiError(413, "too_large", "Anfrage zu groß.");
                }
            }
            String text = buf.toString(StandardCharsets.UTF_8);
            return text.isBlank() ? new LinkedHashMap<>() : Json.parseObject(text);
        }
    }

    private static void send(HttpExchange ex, int status, Object json) throws IOException {
        sendText(ex, status, "application/json; charset=utf-8", Json.write(json));
    }

    private static void sendText(HttpExchange ex, int status, String type, String text) throws IOException {
        byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        ex.getResponseHeaders().set("Content-Type", type);
        ex.getResponseHeaders().set("Cache-Control", "no-cache");
        ex.sendResponseHeaders(status, bytes.length);
        try (OutputStream out = ex.getResponseBody()) {
            out.write(bytes);
        }
    }

    private static void sendEmpty(HttpExchange ex, int status) throws IOException {
        ex.sendResponseHeaders(status, -1);
    }
}
