package de.mirranet.lingua.progress;

import de.mirranet.lingua.content.CourseRepository;
import de.mirranet.lingua.content.CourseRepository.Course;
import de.mirranet.lingua.db.Database;
import de.mirranet.lingua.db.Database.SkillProgress;
import de.mirranet.lingua.util.Json;

import java.sql.Connection;
import java.sql.SQLException;
import java.time.LocalDate;
import java.time.format.DateTimeParseException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.regex.Pattern;

/**
 * Applies learning events sent by the app.
 * <p>
 * The app records an event whenever a lesson or practice session is finished (also offline) and uploads
 * its queue later. Every event carries a client-generated id, so uploading the same event twice
 * (for example after a timeout) is harmless. The server is the source of truth for progress.
 */
public final class ProgressService {

    public static final int MAX_EVENTS_PER_REQUEST = 200;
    public static final int MAX_XP_PER_EVENT = 30;
    private static final Pattern EVENT_ID = Pattern.compile("[A-Za-z0-9_-]{8,64}");

    private final Database db;
    private final CourseRepository courses;

    public ProgressService(Database db, CourseRepository courses) {
        this.db = db;
        this.courses = courses;
    }

    public record Result(int applied, int duplicates, List<Map<String, Object>> rejected) {
    }

    public Result apply(long userId, List<Object> events) throws SQLException {
        List<Map<String, Object>> rejected = new ArrayList<>();
        int[] counts = new int[2];
        db.transaction(c -> {
            for (Object o : events) {
                if (!(o instanceof Map<?, ?> raw)) {
                    rejected.add(Map.of("id", "", "reason", "event must be an object"));
                    continue;
                }
                @SuppressWarnings("unchecked") Map<String, Object> e = (Map<String, Object>) raw;
                String id = Json.str(e, "id");
                String error = validate(e);
                if (error != null) {
                    rejected.add(Map.of("id", id == null ? "" : id, "reason", error));
                    continue;
                }
                if (!Database.markEvent(c, userId, id)) {
                    counts[1]++;
                    continue;
                }
                applyOne(c, userId, e);
                counts[0]++;
            }
            return null;
        });
        return new Result(counts[0], counts[1], rejected);
    }

    /** @return an error message, or null if the event is valid. */
    private String validate(Map<String, Object> e) {
        String id = Json.str(e, "id");
        if (id == null || !EVENT_ID.matcher(id).matches()) {
            return "invalid id";
        }
        String type = Json.str(e, "type");
        Course course = courses.get(Json.str(e, "course") == null ? "" : Json.str(e, "course"));
        if (course == null) {
            return "unknown course";
        }
        if (!"reset".equals(type)) {
            Long xp = Json.num(e, "xp");
            if (xp == null || xp < 0 || xp > MAX_XP_PER_EVENT) {
                return "xp must be 0.." + MAX_XP_PER_EVENT;
            }
            String day = Json.str(e, "day");
            try {
                LocalDate d = LocalDate.parse(day == null ? "" : day);
                LocalDate today = LocalDate.now();
                // The app uses the phone's local date; allow time zones and offline queues, reject nonsense.
                if (d.isAfter(today.plusDays(1)) || d.isBefore(today.minusYears(1))) {
                    return "day out of range";
                }
            } catch (DateTimeParseException ex) {
                return "day must be yyyy-MM-dd";
            }
        }
        switch (type == null ? "" : type) {
            case "lesson" -> {
                String skill = Json.str(e, "skill");
                Integer lessons = skill == null ? null : course.lessonsPerSkill().get(skill);
                if (lessons == null) {
                    return "unknown skill";
                }
                Long lesson = Json.num(e, "lesson");
                if (lesson == null || lesson < 0 || lesson >= lessons) {
                    return "lesson out of range";
                }
            }
            case "practice" -> {
                List<Object> skills = Json.arr(e, "skills");
                if (skills == null || skills.isEmpty() || skills.size() > 20) {
                    return "skills must be a list of 1..20 skill ids";
                }
                for (Object s : skills) {
                    if (!(s instanceof String sid) || !course.lessonsPerSkill().containsKey(sid)) {
                        return "unknown skill in practice";
                    }
                }
            }
            case "reset" -> {
                // nothing else to check
            }
            default -> {
                return "unknown type";
            }
        }
        return null;
    }

    private void applyOne(Connection c, long userId, Map<String, Object> e) throws SQLException {
        String type = Json.str(e, "type");
        String courseId = Json.str(e, "course");
        Course course = courses.get(courseId);
        long now = System.currentTimeMillis();
        long at = clamp(Json.num(e, "at"), now);

        switch (type) {
            case "lesson" -> {
                String skill = Json.str(e, "skill");
                int total = course.lessonsPerSkill().get(skill);
                int lesson = Json.num(e, "lesson").intValue();
                SkillProgress p = orEmpty(Database.readSkill(c, userId, courseId, skill));
                boolean wasDone = p.lessons() >= total;
                int lessons = Math.max(p.lessons(), lesson + 1);
                int reps = p.reps();
                long last = p.lastPracticed();
                if (!wasDone && lessons >= total) {
                    reps = Math.max(1, reps);
                } else if (wasDone) {
                    reps++;
                }
                if (lessons >= total) {
                    last = Math.max(last, at);
                }
                Database.writeSkill(c, userId, courseId, skill, new SkillProgress(lessons, last, reps));
            }
            case "practice" -> {
                for (Object s : Json.arr(e, "skills")) {
                    String skill = (String) s;
                    SkillProgress p = Database.readSkill(c, userId, courseId, skill);
                    if (p != null) {
                        Database.writeSkill(c, userId, courseId, skill,
                                new SkillProgress(p.lessons(), Math.max(p.lastPracticed(), at), p.reps() + 1));
                    }
                }
            }
            case "reset" -> {
                try (var ps = c.prepareStatement("DELETE FROM skill_progress WHERE user_id = ? AND course = ?")) {
                    ps.setLong(1, userId);
                    ps.setString(2, courseId);
                    ps.executeUpdate();
                }
                return; // no XP for a reset
            }
            default -> throw new IllegalStateException("unreachable");
        }
        long xp = Json.num(e, "xp");
        if (xp > 0) {
            Database.addXp(c, userId, Json.str(e, "day"), xp);
        }
    }

    private static long clamp(Long at, long now) {
        if (at == null || at <= 0 || at > now) {
            return now;
        }
        return at;
    }

    private static SkillProgress orEmpty(SkillProgress p) {
        return p == null ? new SkillProgress(0, 0, 0) : p;
    }

    /** Full progress document returned to the app. */
    public Map<String, Object> snapshot(long userId) throws SQLException {
        Map<String, Object> coursesOut = new LinkedHashMap<>();
        for (var ce : db.progress(userId).entrySet()) {
            Map<String, Object> skills = new LinkedHashMap<>();
            for (var se : ce.getValue().entrySet()) {
                SkillProgress p = se.getValue();
                skills.put(se.getKey(), Map.of("lessons", p.lessons(), "lastPracticed", p.lastPracticed(), "reps", p.reps()));
            }
            coursesOut.put(ce.getKey(), skills);
        }
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("courses", coursesOut);
        out.put("xp", db.xp(userId));
        out.put("serverTime", System.currentTimeMillis());
        return out;
    }
}
