package de.mirranet.lingua;

import de.mirranet.lingua.auth.PasswordHasher;
import de.mirranet.lingua.auth.RateLimiter;
import de.mirranet.lingua.content.CourseRepository;
import de.mirranet.lingua.util.Json;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class UnitTest {

    @Test
    void jsonRoundTrip() {
        String text = "{\"a\":[1,2.5,\"x\\\"y\\u00e9\",true,null,{}],\"b\":-3,\"ü\":\"ä\\n\"}";
        Map<String, Object> m = Json.parseObject(text);
        assertEquals(1L, Json.arr(m, "a").get(0));
        assertEquals(2.5, Json.arr(m, "a").get(1));
        assertEquals("x\"yé", Json.arr(m, "a").get(2));
        assertEquals(-3L, m.get("b"));
        assertEquals(m, Json.parseObject(Json.write(m)));
        assertThrows(Json.JsonException.class, () -> Json.parse("{\"a\":1,}"));
        assertThrows(Json.JsonException.class, () -> Json.parse("[1 2]"));
        assertThrows(Json.JsonException.class, () -> Json.parse("{} x"));
    }

    @Test
    void passwordHashing() {
        String h = PasswordHasher.hash("geheim123");
        assertTrue(PasswordHasher.verify("geheim123", h));
        assertFalse(PasswordHasher.verify("geheim124", h));
        assertFalse(PasswordHasher.verify("geheim123", "garbage"));
        assertFalse(PasswordHasher.hash("geheim123").equals(h), "salted");
    }

    @Test
    void rateLimiter() {
        RateLimiter r = new RateLimiter(3, 60_000);
        for (int i = 0; i < 3; i++) {
            assertFalse(r.isBlocked("k"));
            r.recordFailure("k");
        }
        assertTrue(r.isBlocked("k"));
        r.reset("k");
        assertFalse(r.isBlocked("k"));
    }

    @Test
    void bundledCoursesAreValid() throws IOException {
        for (String id : List.of("en", "fr")) {
            try (InputStream in = UnitTest.class.getResourceAsStream("/courses/" + id + ".json")) {
                CourseRepository.Course c = CourseRepository.parse(id, new String(in.readAllBytes(), StandardCharsets.UTF_8));
                assertEquals(21, c.lessonsPerSkill().size());
                assertEquals(3, c.lessonsPerSkill().get("wor"));
                assertEquals(3, c.lessonsPerSkill().get("g1"));
            }
        }
    }

    @Test
    void accuracyBlending() {
        double[] first = de.mirranet.lingua.progress.ProgressService.blend(0, 0, 8, 10);
        assertEquals(0.8, first[0], 1e-9);
        assertEquals(10, (int) first[1]);
        double[] second = de.mirranet.lingua.progress.ProgressService.blend(0.8, 10, 10, 10);
        assertEquals(0.8 * 0.65 + 0.35, second[0], 1e-9);
        assertEquals(20, (int) second[1]);
        double[] none = de.mirranet.lingua.progress.ProgressService.blend(0.5, 4, 0, 0);
        assertEquals(0.5, none[0], 1e-9);
    }

    @Test
    void invalidCoursesAreRejected() {
        assertThrows(IllegalArgumentException.class, () -> CourseRepository.parse("en", "{\"id\":\"fr\",\"name\":\"x\",\"tts\":\"fr\",\"skills\":[]}"));
        assertThrows(IllegalArgumentException.class, () -> CourseRepository.parse("en", "{\"id\":\"en\",\"name\":\"x\",\"tts\":\"en\",\"skills\":[]}"));
        assertThrows(IllegalArgumentException.class, () -> CourseRepository.parse("en",
                "{\"id\":\"en\",\"name\":\"x\",\"tts\":\"en\",\"skills\":[{\"id\":\"a\",\"title\":\"A\",\"row\":0,\"sentences\":[{\"t\":[],\"de\":[\"x\"]}]}]}"));
        CourseRepository.Course ok = CourseRepository.parse("en",
                "{\"id\":\"en\",\"name\":\"x\",\"tts\":\"en\",\"lessonSize\":2,\"skills\":[{\"id\":\"a\",\"title\":\"A\",\"row\":0,"
                        + "\"sentences\":[{\"t\":[\"a\"],\"de\":[\"b\"]},{\"t\":[\"a\"],\"de\":[\"b\"]},{\"t\":[\"a\"],\"de\":[\"b\"]}]}]}");
        assertEquals(2, ok.lessonsPerSkill().get("a"));
    }
}
