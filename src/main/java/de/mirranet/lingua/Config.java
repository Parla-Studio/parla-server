package de.mirranet.lingua;

import java.io.IOException;
import java.io.InputStream;
import java.io.Reader;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Properties;

/**
 * Server configuration, read from {@code mirra-lingua.properties} in the working directory.
 * A commented default file is written on first start.
 */
public record Config(
        String bind,
        int port,
        Path database,
        Path contentDir,
        boolean registrationOpen,
        int sessionDays,
        boolean trustProxy) {

    public static Config load(Path file) throws IOException {
        if (!Files.exists(file)) {
            try (InputStream in = Config.class.getResourceAsStream("/mirra-lingua.default.properties")) {
                if (in != null) {
                    Files.copy(in, file);
                }
            }
        }
        Properties p = new Properties();
        if (Files.exists(file)) {
            try (Reader r = Files.newBufferedReader(file, StandardCharsets.UTF_8)) {
                p.load(r);
            }
        }
        return new Config(
                p.getProperty("bind", "127.0.0.1").trim(),
                Integer.parseInt(p.getProperty("port", "8085").trim()),
                Path.of(p.getProperty("database", "data/mirra-lingua.db").trim()),
                Path.of(p.getProperty("contentDir", "content").trim()),
                Boolean.parseBoolean(p.getProperty("registrationOpen", "true").trim()),
                Integer.parseInt(p.getProperty("sessionDays", "180").trim()),
                Boolean.parseBoolean(p.getProperty("trustProxy", "true").trim()));
    }

    /** Writes this configuration back to disk (used by tests and tooling). */
    public void save(Path file) throws IOException {
        Properties p = new Properties();
        p.setProperty("bind", bind);
        p.setProperty("port", Integer.toString(port));
        p.setProperty("database", database.toString());
        p.setProperty("contentDir", contentDir.toString());
        p.setProperty("registrationOpen", Boolean.toString(registrationOpen));
        p.setProperty("sessionDays", Integer.toString(sessionDays));
        p.setProperty("trustProxy", Boolean.toString(trustProxy));
        try (Writer w = Files.newBufferedWriter(file, StandardCharsets.UTF_8)) {
            p.store(w, "MirraLingua server configuration");
        }
    }
}
