package de.mirranet.lingua;

import com.sun.net.httpserver.HttpServer;
import de.mirranet.lingua.api.Api;
import de.mirranet.lingua.content.CourseRepository;
import de.mirranet.lingua.db.Database;
import de.mirranet.lingua.progress.ProgressService;

import java.net.InetSocketAddress;
import java.nio.file.Path;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.logging.Level;
import java.util.logging.Logger;

/** Entry point: {@code java -jar mirra-lingua-server.jar [config file]}. */
public final class MirraLinguaServer {

    static {
        // Must run before the first logger is created.
        System.setProperty("java.util.logging.SimpleFormatter.format", "%1$tF %1$tT %4$-7s [%3$s] %5$s%6$s%n");
    }

    private static final Logger LOG = Logger.getLogger(MirraLinguaServer.class.getName());
    public static final String VERSION = "1.0.0";

    private final HttpServer http;
    private final Database db;
    private final ScheduledExecutorService maintenance;

    public MirraLinguaServer(Config config) throws Exception {
        db = new Database(config.database());
        CourseRepository courses = new CourseRepository(config.contentDir());
        ProgressService progress = new ProgressService(db, courses);

        http = HttpServer.create(new InetSocketAddress(config.bind(), config.port()), 64);
        http.createContext("/", new Api(config, db, courses, progress, VERSION));
        http.setExecutor(Executors.newVirtualThreadPerTaskExecutor());

        long sessionMillis = config.sessionDays() * 86_400_000L;
        maintenance = Executors.newSingleThreadScheduledExecutor(r -> {
            Thread t = new Thread(r, "maintenance");
            t.setDaemon(true);
            return t;
        });
        maintenance.scheduleAtFixedRate(() -> {
            try {
                int s = db.purgeIdleSessions(sessionMillis);
                int e = db.purgeOldEvents(400L * 86_400_000L);
                if (s + e > 0) {
                    LOG.info("Maintenance: removed " + s + " idle session(s), " + e + " old event id(s)");
                }
            } catch (Exception ex) {
                LOG.log(Level.WARNING, "Maintenance failed", ex);
            }
        }, 1, 24 * 60, TimeUnit.MINUTES);

        LOG.info("Content directory: " + courses.dir().toAbsolutePath() + " (" + courses.list().size() + " course(s))");
    }

    public void start() {
        http.start();
        LOG.info("MirraLingua server " + VERSION + " listening on " + http.getAddress());
    }

    public int port() {
        return http.getAddress().getPort();
    }

    public void stop() {
        http.stop(1);
        maintenance.shutdownNow();
        try {
            db.close();
        } catch (Exception e) {
            LOG.log(Level.WARNING, "Closing database failed", e);
        }
    }

    public static void main(String[] args) throws Exception {
        Path configFile = Path.of(args.length > 0 ? args[0] : "mirra-lingua.properties");
        Config config = Config.load(configFile);
        MirraLinguaServer server = new MirraLinguaServer(config);
        Runtime.getRuntime().addShutdownHook(new Thread(server::stop, "shutdown"));
        server.start();
    }
}
