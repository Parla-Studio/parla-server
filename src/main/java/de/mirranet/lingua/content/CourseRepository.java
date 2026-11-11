package de.mirranet.lingua.content;

import de.mirranet.lingua.auth.Tokens;
import de.mirranet.lingua.util.Json;

import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.URISyntaxException;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.FileSystem;
import java.nio.file.FileSystemAlreadyExistsException;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.regex.Pattern;
import java.util.stream.Stream;

/**
 * Loads course files ({@code <id>.json}) from the content directory and reloads them when they change on disk.
 * This is what lets new lessons reach the app without an app update: edit or add a file, and the next
 * content check from the app sees a new hash and downloads it.
 * <p>
 * A file that fails validation is logged and ignored; the previously loaded version stays active.
 * A course with {@code "published": false} is loaded but not offered to the app (useful for drafts).
 */
public final class CourseRepository {

    private static final Logger LOG = Logger.getLogger(CourseRepository.class.getName());
    private static final Pattern ID = Pattern.compile("[a-z0-9][a-z0-9_-]{0,31}");
    private static final long CHECK_INTERVAL_MS = 5_000;

    /** A validated, loaded course. {@code json} is the exact document served to the app. */
    public record Course(String id, String name, String flag, long version, String hash, boolean published,
                         int order, String json, Map<String, Integer> lessonsPerSkill) {
    }

    private record FileStamp(long modified, long size) {
    }

    private final Path dir;
    private volatile Map<String, Course> courses = Map.of();
    private final Map<Path, FileStamp> stamps = new HashMap<>();
    private long lastCheck;

    public CourseRepository(Path dir) throws IOException {
        this.dir = dir;
        if (!Files.isDirectory(dir)) {
            Files.createDirectories(dir);
            copyBundledCourses(dir);
        }
        reload(true);
    }

    public Path dir() {
        return dir;
    }

    /** Published courses in display order. */
    public List<Course> list() {
        reloadIfDue();
        List<Course> out = new ArrayList<>();
        for (Course c : courses.values()) {
            if (c.published()) {
                out.add(c);
            }
        }
        out.sort(Comparator.comparingInt(Course::order).thenComparing(Course::id));
        return out;
    }

    /** Any loaded course, published or not (progress for an unpublished course is still accepted). */
    public Course get(String id) {
        reloadIfDue();
        return courses.get(id);
    }

    private synchronized void reloadIfDue() {
        long now = System.currentTimeMillis();
        if (now - lastCheck >= CHECK_INTERVAL_MS) {
            lastCheck = now;
            try {
                reload(false);
            } catch (IOException e) {
                LOG.log(Level.WARNING, "Could not scan content directory " + dir, e);
            }
        }
    }

    /** Rescans the content directory; only changed files are parsed again. */
    public synchronized void reload(boolean force) throws IOException {
        lastCheck = System.currentTimeMillis();
        Map<String, Course> next = new LinkedHashMap<>(courses);
        Map<Path, FileStamp> seen = new HashMap<>();
        boolean changed = false;

        List<Path> files;
        try (Stream<Path> s = Files.list(dir)) {
            files = s.filter(p -> p.getFileName().toString().endsWith(".json")).toList();
        }
        for (Path file : files) {
            FileStamp stamp = new FileStamp(Files.getLastModifiedTime(file).toMillis(), Files.size(file));
            seen.put(file, stamp);
            if (!force && stamp.equals(stamps.get(file))) {
                continue;
            }
            String expectedId = file.getFileName().toString().replaceFirst("\\.json$", "");
            try {
                Course c = parse(expectedId, Files.readString(file, StandardCharsets.UTF_8));
                Course old = next.put(c.id(), c);
                changed = true;
                if (old == null) {
                    LOG.info("Loaded course '" + c.id() + "' (" + c.lessonsPerSkill().size() + " skills, hash " + c.hash() + ")");
                } else if (!old.hash().equals(c.hash())) {
                    LOG.info("Updated course '" + c.id() + "' (hash " + old.hash() + " -> " + c.hash() + ")");
                }
            } catch (RuntimeException | IOException e) {
                LOG.warning("Ignoring invalid course file " + file + ": " + e.getMessage()
                        + (next.containsKey(expectedId) ? " (keeping previous version)" : ""));
            }
        }
        // Courses whose file was deleted disappear.
        for (Path gone : new ArrayList<>(stamps.keySet())) {
            if (!seen.containsKey(gone)) {
                String id = gone.getFileName().toString().replaceFirst("\\.json$", "");
                if (next.remove(id) != null) {
                    LOG.info("Removed course '" + id + "' (file deleted)");
                    changed = true;
                }
            }
        }
        stamps.clear();
        stamps.putAll(seen);
        if (changed || force) {
            courses = Collections.unmodifiableMap(next);
        }
    }

    /** Parses and validates a course document. Throws {@link IllegalArgumentException} with a readable reason. */
    public static Course parse(String expectedId, String text) {
        Map<String, Object> root;
        try {
            root = Json.parseObject(text);
        } catch (Json.JsonException e) {
            throw new IllegalArgumentException("not valid JSON: " + e.getMessage());
        }
        String id = req(Json.str(root, "id"), "id");
        if (!ID.matcher(id).matches()) {
            throw new IllegalArgumentException("id must match " + ID.pattern());
        }
        if (expectedId != null && !expectedId.equals(id)) {
            throw new IllegalArgumentException("id '" + id + "' does not match file name '" + expectedId + "'");
        }
        String name = req(Json.str(root, "name"), "name");
        req(Json.str(root, "tts"), "tts");
        long lessonSize = orDefault(Json.num(root, "lessonSize"), 4L);
        if (lessonSize < 1 || lessonSize > 20) {
            throw new IllegalArgumentException("lessonSize must be 1..20");
        }
        List<Object> skills = Json.arr(root, "skills");
        if (skills == null || skills.isEmpty()) {
            throw new IllegalArgumentException("skills must be a non-empty list");
        }
        Map<String, Integer> lessons = new LinkedHashMap<>();
        for (int i = 0; i < skills.size(); i++) {
            if (!(skills.get(i) instanceof Map<?, ?> rawSkill)) {
                throw new IllegalArgumentException("skills[" + i + "] must be an object");
            }
            @SuppressWarnings("unchecked") Map<String, Object> skill = (Map<String, Object>) rawSkill;
            String where = "skills[" + i + "]";
            String sid = req(Json.str(skill, "id"), where + ".id");
            if (!ID.matcher(sid).matches()) {
                throw new IllegalArgumentException(where + ".id must match " + ID.pattern());
            }
            if (lessons.containsKey(sid)) {
                throw new IllegalArgumentException("duplicate skill id '" + sid + "'");
            }
            req(Json.str(skill, "title"), where + ".title");
            Object level = skill.get("level");
            if (level != null && !(level instanceof String)) {
                throw new IllegalArgumentException(where + ".level must be a string such as \"A2\"");
            }
            if (Json.num(skill, "row") == null) {
                throw new IllegalArgumentException(where + ".row is required");
            }
            List<Object> sentences = Json.arr(skill, "sentences");
            if (sentences == null || sentences.isEmpty()) {
                throw new IllegalArgumentException(where + ".sentences must be a non-empty list");
            }
            for (int j = 0; j < sentences.size(); j++) {
                if (!(sentences.get(j) instanceof Map<?, ?> s)
                        || !nonEmptyStrings(s.get("t")) || !nonEmptyStrings(s.get("de"))) {
                    throw new IllegalArgumentException(where + ".sentences[" + j + "] needs non-empty 't' and 'de' lists");
                }
            }
            List<Object> words = Json.arr(skill, "words");
            if (words != null) {
                for (int j = 0; j < words.size(); j++) {
                    if (!(words.get(j) instanceof List<?> w) || w.size() != 2
                            || !(w.get(0) instanceof String) || !(w.get(1) instanceof String)) {
                        throw new IllegalArgumentException(where + ".words[" + j + "] must be [\"target\", \"german\"]");
                    }
                }
            }
            lessons.put(sid, (int) ((sentences.size() + lessonSize - 1) / lessonSize));
        }
        boolean published = !Boolean.FALSE.equals(Json.bool(root, "published"));
        long version = orDefault(Json.num(root, "version"), 1L);
        int order = orDefault(Json.num(root, "order"), 100L).intValue();
        String hash = Tokens.sha256(text).substring(0, 16);
        return new Course(id, name, orDefault(Json.str(root, "flag"), ""), version, hash, published, order, text,
                Collections.unmodifiableMap(lessons));
    }

    private static boolean nonEmptyStrings(Object o) {
        if (!(o instanceof List<?> l) || l.isEmpty()) {
            return false;
        }
        for (Object x : l) {
            if (!(x instanceof String s) || s.isBlank()) {
                return false;
            }
        }
        return true;
    }

    private static <T> T req(T v, String field) {
        if (v == null || (v instanceof String s && s.isBlank())) {
            throw new IllegalArgumentException(field + " is required");
        }
        return v;
    }

    private static <T> T orDefault(T v, T def) {
        return v == null ? def : v;
    }

    /** Seeds an empty content directory with the courses that ship inside the JAR. */
    private static void copyBundledCourses(Path target) throws IOException {
        URL url = CourseRepository.class.getResource("/courses");
        if (url == null) {
            return;
        }
        try {
            URI uri = url.toURI();
            if ("jar".equals(uri.getScheme())) {
                FileSystem fs;
                try {
                    fs = FileSystems.newFileSystem(uri, Map.of());
                } catch (FileSystemAlreadyExistsException e) {
                    fs = FileSystems.getFileSystem(uri);
                }
                copyJsonFiles(fs.getPath("/courses"), target);
            } else {
                copyJsonFiles(Path.of(uri), target);
            }
        } catch (URISyntaxException e) {
            throw new IOException(e);
        }
    }

    private static void copyJsonFiles(Path from, Path to) throws IOException {
        try (Stream<Path> s = Files.list(from)) {
            for (Path p : s.filter(f -> f.getFileName().toString().endsWith(".json")).toList()) {
                try (InputStream in = Files.newInputStream(p)) {
                    Files.copy(in, to.resolve(p.getFileName().toString()));
                }
                LOG.info("Installed bundled course " + p.getFileName());
            }
        }
    }
}
