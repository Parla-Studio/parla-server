package de.mirranet.lingua;

import de.mirranet.lingua.util.Json;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.FileTime;
import java.time.LocalDate;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Starts a real server on a random port with a temporary database and talks to it over HTTP. */
class ApiIntegrationTest {

    @TempDir
    Path tmp;
    private MirraLinguaServer server;
    private final HttpClient http = HttpClient.newHttpClient();
    private String base;

    @BeforeEach
    void start() throws Exception {
        Config config = new Config("127.0.0.1", 0, tmp.resolve("db/test.db"), tmp.resolve("content"), true, 30, false);
        server = new MirraLinguaServer(config);
        server.start();
        base = "http://127.0.0.1:" + server.port();
    }

    @AfterEach
    void stop() {
        server.stop();
    }

    private record Res(int status, Map<String, Object> json, HttpResponse<String> raw) {
    }

    private Res call(String method, String path, Object body, String token, String etag) throws Exception {
        HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(base + path))
                .method(method, body == null ? HttpRequest.BodyPublishers.noBody()
                        : HttpRequest.BodyPublishers.ofString(Json.write(body)));
        if (token != null) {
            b.header("Authorization", "Bearer " + token);
        }
        if (etag != null) {
            b.header("If-None-Match", "\"" + etag + "\"");
        }
        HttpResponse<String> r = http.send(b.build(), HttpResponse.BodyHandlers.ofString());
        Map<String, Object> json = r.body().isBlank() || !r.body().startsWith("{") ? Map.of() : Json.parseObject(r.body());
        return new Res(r.statusCode(), json, r);
    }

    private Res call(String method, String path, Object body, String token) throws Exception {
        return call(method, path, body, token, null);
    }

    private String register(String user) throws Exception {
        Res r = call("POST", "/api/auth/register", Map.of("username", user, "password", "geheim123", "device", "test"), null);
        assertEquals(201, r.status(), r.raw().body());
        return (String) r.json().get("token");
    }

    @Test
    void registerLoginLogout() throws Exception {
        String token = register("niklas");
        assertEquals(409, call("POST", "/api/auth/register", Map.of("username", "NIKLAS", "password", "geheim123"), null).status());
        assertEquals(400, call("POST", "/api/auth/register", Map.of("username", "x", "password", "geheim123"), null).status());
        assertEquals(400, call("POST", "/api/auth/register", Map.of("username", "anna", "password", "kurz"), null).status());

        assertEquals(401, call("POST", "/api/auth/login", Map.of("username", "niklas", "password", "falsch!!"), null).status());
        Res login = call("POST", "/api/auth/login", Map.of("username", "Niklas", "password", "geheim123"), null);
        assertEquals(200, login.status());
        String second = (String) login.json().get("token");
        assertNotEquals(token, second);

        Res me = call("GET", "/api/me", null, second);
        assertEquals(200, me.status());
        assertEquals("niklas", Json.obj(me.json(), "user").get("username"));

        assertEquals(204, call("POST", "/api/auth/logout", null, second).status());
        assertEquals(401, call("GET", "/api/me", null, second).status());
        assertEquals(200, call("GET", "/api/me", null, token).status());
        assertEquals(401, call("GET", "/api/progress", null, null).status());
    }

    @Test
    void loginIsRateLimited() throws Exception {
        register("brute");
        int last = 0;
        for (int i = 0; i < 10; i++) {
            last = call("POST", "/api/auth/login", Map.of("username", "brute", "password", "wrong-pass"), null).status();
        }
        assertEquals(429, last);
    }

    @Test
    void coursesAreServedWithEtags() throws Exception {
        Res list = call("GET", "/api/courses", null, null);
        assertEquals(200, list.status());
        List<Object> courses = Json.arr(list.json(), "courses");
        assertEquals(2, courses.size());
        String listHash = (String) list.json().get("hash");
        assertEquals(304, call("GET", "/api/courses", null, null, listHash).status());

        Res en = call("GET", "/api/courses/en", null, null);
        assertEquals(200, en.status());
        assertEquals("en", en.json().get("id"));
        String hash = en.raw().headers().firstValue("X-Course-Hash").orElseThrow();
        assertEquals(304, call("GET", "/api/courses/en", null, null, hash).status());
        assertEquals(404, call("GET", "/api/courses/xx", null, null).status());
    }

    @Test
    void editedCourseFileIsPickedUpWithoutRestart() throws Exception {
        String before = (String) call("GET", "/api/courses", null, null).json().get("hash");
        Path file = tmp.resolve("content/fr.json");
        String text = Files.readString(file).replaceFirst("\"version\":\\s*\\d+", "\"version\": 99");
        Files.writeString(file, text);
        Files.setLastModifiedTime(file, FileTime.fromMillis(System.currentTimeMillis() + 10_000));
        Thread.sleep(5_200); // reload check interval

        Res list = call("GET", "/api/courses", null, null);
        assertNotEquals(before, list.json().get("hash"));

        // A broken file must not take the course offline.
        Files.writeString(file, "{ broken");
        Files.setLastModifiedTime(file, FileTime.fromMillis(System.currentTimeMillis() + 20_000));
        Thread.sleep(5_200);
        assertEquals(200, call("GET", "/api/courses/fr", null, null).status());
    }

    @Test
    void progressEventsAreIdempotent() throws Exception {
        String token = register("lerner");
        String today = LocalDate.now().toString();
        List<Object> events = List.of(
                Map.of("id", "evt-000001", "type", "lesson", "course", "en", "skill", "g1", "lesson", 0, "xp", 13, "day", today,
                        "at", System.currentTimeMillis(), "correct", 8, "total", 10),
                Map.of("id", "evt-000002", "type", "lesson", "course", "en", "skill", "g1", "lesson", 1, "xp", 12, "day", today),
                Map.of("id", "evt-000003", "type", "lesson", "course", "en", "skill", "g1", "lesson", 2, "xp", 11, "day", today),
                Map.of("id", "evt-bad-001", "type", "lesson", "course", "en", "skill", "g1", "lesson", 99, "xp", 10, "day", today),
                Map.of("id", "evt-bad-002", "type", "lesson", "course", "en", "skill", "g1", "lesson", 0, "xp", 999, "day", today),
                Map.of("id", "evt-bad-003", "type", "lesson", "course", "en", "skill", "g1", "lesson", 0, "xp", 10, "day", today,
                        "correct", 5, "total", 3));
        Res r = call("POST", "/api/progress/events", Map.of("events", events), token);
        assertEquals(200, r.status(), r.raw().body());
        assertEquals(3L, r.json().get("applied"));
        assertEquals(3, Json.arr(r.json(), "rejected").size());

        Map<String, Object> g1 = Json.obj(Json.obj(Json.obj(Json.obj(r.json(), "progress"), "courses"), "en"), "g1");
        assertEquals(3L, g1.get("lessons"));
        assertEquals(1L, g1.get("reps"));
        assertTrue((Long) g1.get("lastPracticed") > 0);
        assertEquals(0.8, ((Number) g1.get("accuracy")).doubleValue(), 1e-9);
        assertEquals(10L, g1.get("answers"));
        assertEquals(36L, Json.obj(Json.obj(r.json(), "progress"), "xp").get(today));

        // Same upload again: nothing changes.
        Res again = call("POST", "/api/progress/events", Map.of("events", events), token);
        assertEquals(0L, again.json().get("applied"));
        assertEquals(3L, again.json().get("duplicates"));
        assertEquals(36L, Json.obj(Json.obj(again.json(), "progress"), "xp").get(today));

        // Practice strengthens, reset clears the course.
        call("POST", "/api/progress/events", Map.of("events", List.of(
                Map.of("id", "evt-000004", "type", "practice", "course", "en", "skills", List.of("g1"), "xp", 10, "day", today,
                        "correct", 10, "total", 10))), token);
        Map<String, Object> snap = call("GET", "/api/progress", null, token).json();
        Map<String, Object> g1After = Json.obj(Json.obj(Json.obj(snap, "courses"), "en"), "g1");
        assertEquals(2L, g1After.get("reps"));
        assertEquals(0.87, ((Number) g1After.get("accuracy")).doubleValue(), 1e-9);
        assertEquals(20L, g1After.get("answers"));
        call("POST", "/api/progress/events", Map.of("events", List.of(
                Map.of("id", "evt-000005", "type", "reset", "course", "en"))), token);
        snap = call("GET", "/api/progress", null, token).json();
        assertTrue(Json.obj(snap, "courses").isEmpty());
        assertEquals(46L, Json.obj(snap, "xp").get(today));
    }

    @Test
    void settingsPasswordAndAccountDeletion() throws Exception {
        String token = register("settings");
        Res s = call("PUT", "/api/settings", Map.of("goal", 30, "sound", false, "course", "fr"), token);
        assertEquals(200, s.status());
        assertEquals(30L, s.json().get("goal"));
        assertEquals(400, call("PUT", "/api/settings", Map.of("goal", 7), token).status());
        assertEquals("fr", Json.obj(call("GET", "/api/me", null, token).json(), "settings").get("course"));

        String other = (String) call("POST", "/api/auth/login", Map.of("username", "settings", "password", "geheim123"), null).json().get("token");
        assertEquals(403, call("POST", "/api/auth/password", Map.of("oldPassword", "nope-nope", "newPassword", "neuesPasswort"), token).status());
        assertEquals(204, call("POST", "/api/auth/password", Map.of("oldPassword", "geheim123", "newPassword", "neuesPasswort"), token).status());
        assertEquals(401, call("GET", "/api/me", null, other).status(), "other devices are signed out");
        assertEquals(200, call("POST", "/api/auth/login", Map.of("username", "settings", "password", "neuesPasswort"), null).status());

        assertEquals(204, call("DELETE", "/api/account", Map.of("password", "neuesPasswort"), token).status());
        assertEquals(401, call("GET", "/api/me", null, token).status());
        assertEquals(401, call("POST", "/api/auth/login", Map.of("username", "settings", "password", "neuesPasswort"), null).status());
    }

    @Test
    void health() throws Exception {
        Res r = call("GET", "/api/health", null, null);
        assertEquals(200, r.status());
        assertNotNull(r.json().get("version"));
    }
}
