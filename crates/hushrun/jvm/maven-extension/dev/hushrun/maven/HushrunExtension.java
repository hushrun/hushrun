package dev.hushrun.maven;

import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Deque;
import java.util.HashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Properties;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import javax.inject.Named;
import javax.inject.Singleton;
import org.apache.maven.AbstractMavenLifecycleParticipant;
import org.apache.maven.execution.MavenSession;
import org.apache.maven.model.Dependency;
import org.apache.maven.model.Plugin;
import org.apache.maven.model.PluginExecution;
import org.apache.maven.project.MavenProject;
import org.codehaus.plexus.util.xml.Xpp3Dom;

/**
 * hushrun's Maven core extension, loaded with {@code -Dmaven.ext.class.path=<this jar>} when the
 * variable {@code HUSHRUN_UNITS} names hushrun's directory for the run (ADR 0085). After Maven has
 * read the reactor, before any plugin runs, it:
 *
 * <ul>
 *   <li>writes what Maven's own model says to {@code $HUSHRUN_UNITS/maven-<pid>-<nanos>.json}, a
 *       {@code hushrun.unit-facts/v1} file whose {@code settings.maven} holds every project of the
 *       reactor: its directory, source and test source roots, the reactor projects it depends on
 *       (through dependencies of every scope, its parent, its plugins' dependencies and the
 *       annotation processors its compiler runs), those of them this build doesn't build, its
 *       Surefire's and Failsafe's versions and settings, and the reports already in their reports
 *       directories;
 *   <li>turns {@code hushrun.json}'s {@code reuse} and {@code reuseTests} into the test-JVM hook's
 *       list, {@code $HUSHRUN_UNITS/test-jvm/leave-out.txt}, one {@code @<directory>} section a
 *       project, unless a list is there already.
 * </ul>
 *
 * <p>It changes nothing in the build, and no error of its own ever reaches it: what it can't do,
 * it leaves undone, and says so in its file when it can.
 */
@Named("hushrun")
@Singleton
public class HushrunExtension extends AbstractMavenLifecycleParticipant {
    static final String UNITS = "HUSHRUN_UNITS";
    static final String[] KINDS = {".java", ".kt", ".groovy", ".scala"};
    static final String[] PLUGINS = {"surefire", "failsafe"};
    static final String[] PARAMETERS = {"excludesFile", "includesFile", "test", "includes", "excludes",
        "additionalClasspathElements", "reportsDirectory", "disableXmlReport", "failIfNoTests", "skipTests",
        "skip", "skipExec", "reportNameSuffix", "forkCount", "reuseForks", "parallel", "useModulePath",
        "rerunFailingTestsCount", "testSourceDirectory", "testClassesDirectory"};
    static final String[] PROPERTIES = {"test", "it.test", "skipTests", "skipITs", "maven.test.skip",
        "maven.test.skip.exec", "failIfNoTests", "surefire.failIfNoTests", "failsafe.failIfNoTests",
        "surefire.failIfNoSpecifiedTests", "surefire.reportNameSuffix", "disableXmlReport",
        "maven.test.additionalClasspath", "surefire.excludesFile", "failsafe.excludesFile",
        "surefire.rerunFailingTestsCount", "failsafe.rerunFailingTestsCount", "forkCount", "reuseForks"};

    @Override
    public void afterProjectsRead(MavenSession session) {
        try {
            String units = System.getenv(UNITS);
            if (units == null || units.trim().isEmpty()) return;
            File dir = new File(units.trim());
            if (!dir.isDirectory()) return;
            long started = System.currentTimeMillis();
            List<String> errors = new ArrayList<>();
            List<MavenProject> all = projects(session);
            String leaveOut = null;
            try {
                leaveOut = leaveOut(dir, all, session, errors);
            } catch (Throwable e) {
                errors.add("the list of tests to leave out: " + e);
            }
            write(dir, graph(session, all, started, canonical(dir), leaveOut, errors));
        } catch (Throwable e) {
            // Never the build's problem: without the file, hushrun records nothing of this run.
        }
    }

    /** Every project of the reactor, those this build builds first. */
    private static List<MavenProject> projects(MavenSession session) {
        List<MavenProject> out = new ArrayList<>(session.getProjects());
        try {
            List<MavenProject> every = session.getAllProjects();
            if (every != null) {
                for (MavenProject p : every) if (!out.contains(p)) out.add(p);
            }
        } catch (Throwable e) {
            // An old Maven without getAllProjects: the built ones are all it knows.
        }
        return out;
    }

    private static String graph(MavenSession session, List<MavenProject> all, long started, String unitsDir,
                                String leaveOut, List<String> errors) {
        StringBuilder b = new StringBuilder();
        // One start of Maven: every fork and module of it is this one build.
        String start = pid() + "-" + System.nanoTime();
        b.append("{\"schema\":\"hushrun.unit-facts/v1\",\"adapter\":\"maven\",\"start\":").append(Json.q(start));
        b.append(",\"settings\":{\"maven\":{");
        b.append("\"version\":1,\"mavenVersion\":").append(Json.q(mavenVersion()));
        b.append(",\"startedMs\":").append(started);
        b.append(",\"pid\":").append(Json.q(pid()));
        b.append(",\"unitsDir\":").append(Json.q(unitsDir));
        b.append(",\"leaveOut\":").append(leaveOut == null ? "null" : leaveOut);
        b.append(",\"properties\":{");
        String sep = "";
        for (String name : PROPERTIES) {
            String v = property(session, name);
            if (v == null) continue;
            b.append(sep).append(Json.q(name)).append(':').append(Json.q(v));
            sep = ",";
        }
        b.append("},\"projects\":[");
        Map<String, MavenProject> byId = new HashMap<>();
        for (MavenProject p : all) byId.put(id(p), p);
        for (int i = 0; i < all.size(); i++) {
            MavenProject p = all.get(i);
            boolean built = session.getProjects().contains(p);
            if (i > 0) b.append(',');
            b.append("\n{\"id\":").append(Json.q(id(p)))
             .append(",\"built\":").append(built)
             .append(",\"packaging\":").append(Json.q(p.getPackaging()))
             .append(",\"basedir\":").append(Json.q(canonical(p.getBasedir())))
             .append(",\"sourceRoots\":").append(roots(p.getCompileSourceRoots(), p))
             .append(",\"testSourceRoots\":").append(roots(p.getTestCompileSourceRoots(), p));
            List<String> upstream = new ArrayList<>();
            List<String> notBuilt = new ArrayList<>();
            if (built) {
                try {
                    for (MavenProject u : session.getProjectDependencyGraph().getUpstreamProjects(p, true)) {
                        if (!upstream.contains(id(u))) upstream.add(id(u));
                    }
                } catch (Throwable e) {
                    errors.add(id(p) + ": the reactor's graph: " + e);
                }
                // What the reactor's graph may not hold: the projects its plugins run, an annotation
                // processor among them, and theirs. Its tests read what those generate or check.
                for (MavenProject r : closure(byId, p)) {
                    if (!upstream.contains(id(r))) upstream.add(id(r));
                    if (!session.getProjects().contains(r)) notBuilt.add(id(r));
                }
            }
            b.append(",\"upstream\":").append(strings(upstream))
             .append(",\"upstreamNotBuilt\":").append(strings(notBuilt))
             .append(",\"testJars\":").append(strings(testJars(byId, p)))
             .append(",\"plugins\":{");
            sep = "";
            for (String kind : PLUGINS) {
                Plugin pl = p.getPlugin("org.apache.maven.plugins:maven-" + kind + "-plugin");
                if (pl == null) continue;
                String reports = reportsDir(p, pl, kind);
                b.append(sep).append(Json.q(kind)).append(":{\"version\":").append(Json.q(pl.getVersion()))
                 .append(",\"executions\":").append(pl.getExecutions().size())
                 .append(",\"reportsDirectory\":").append(Json.q(reports))
                 .append(",\"configured\":{");
                String s2 = "";
                for (String param : PARAMETERS) {
                    String v = configured(pl, param);
                    if (v != null) {
                        b.append(s2).append(Json.q(param)).append(':').append(Json.q(v));
                        s2 = ",";
                    }
                }
                b.append("},\"reports\":").append(snapshot(reports)).append('}');
                sep = ",";
            }
            b.append("}}");
        }
        b.append("\n],\"errors\":").append(strings(errors)).append("}}}\n");
        return b.toString();
    }

    /**
     * Writes the test-JVM hook's list from hushrun.json, and says what it did as JSON: the lines
     * written by project, the units it didn't list and why. Null when there is nothing to list.
     */
    private static String leaveOut(File dir, List<MavenProject> all, MavenSession session, List<String> errors)
            throws IOException {
        File run = new File(dir, "hushrun.json");
        if (!run.isFile()) return null;
        Object parsed = Json.parse(new String(Files.readAllBytes(run.toPath()), StandardCharsets.UTF_8));
        if (!(parsed instanceof Map)) throw new IllegalArgumentException("hushrun.json isn't an object");
        Map<?, ?> json = (Map<?, ?>) parsed;
        Object rootValue = json.get("root");
        if (!(rootValue instanceof String)) throw new IllegalArgumentException("hushrun.json has no root");
        File root = new File((String) rootValue);
        // Each unit, with null for the whole class, or the ids of its tests.
        Map<String, Set<String>> wanted = new TreeMap<>();
        Object reuse = json.get("reuse");
        if (reuse instanceof List) {
            for (Object u : (List<?>) reuse) if (u instanceof String) wanted.put((String) u, null);
        }
        Object reuseTests = json.get("reuseTests");
        if ("test".equals(json.get("unit")) && reuseTests instanceof Map) {
            for (Map.Entry<?, ?> e : ((Map<?, ?>) reuseTests).entrySet()) {
                if (!(e.getKey() instanceof String) || !(e.getValue() instanceof List)) continue;
                String unit = (String) e.getKey();
                if (wanted.containsKey(unit) && wanted.get(unit) == null) continue;
                Set<String> ids = new TreeSet<>();
                for (Object id : (List<?>) e.getValue()) if (id instanceof String) ids.add((String) id);
                if (!ids.isEmpty()) wanted.put(unit, ids);
            }
        }
        if (wanted.isEmpty()) return null;

        // Each project's directory and test source roots, canonical.
        Map<String, MavenProject> byDir = new HashMap<>();
        List<String[]> testRoots = new ArrayList<>();  // {root, project dir}
        for (MavenProject p : session.getProjects()) {
            String base = canonical(p.getBasedir());
            byDir.put(base, p);
            for (String r : p.getTestCompileSourceRoots()) testRoots.add(new String[] {canonical(absolute(r, p)), base});
        }
        Map<String, List<String>> lines = new TreeMap<>();      // project dir -> lines
        Map<String, List<String>> unitsOf = new TreeMap<>();    // project dir -> units, sorted
        Map<String, String> classOf = new HashMap<>();
        Map<String, String> dirOf = new HashMap<>();
        List<String[]> kept = new ArrayList<>();
        for (String unit : wanted.keySet()) {
            String[] resolved = resolve(unit, root, byDir, testRoots);
            if (resolved == null) {
                kept.add(new String[] {unit, "it isn't a test class of a project this build builds"});
                continue;
            }
            classOf.put(unit, resolved[1]);
            dirOf.put(unit, resolved[0]);
            List<String> mine = unitsOf.get(resolved[0]);
            if (mine == null) unitsOf.put(resolved[0], mine = new ArrayList<>());
            mine.add(unit);
        }
        for (Map.Entry<String, List<String>> e : unitsOf.entrySet()) {
            MavenProject p = byDir.get(e.getKey());
            List<String> mine = e.getValue();
            Collections.sort(mine);
            int from = 0;
            if (failIfNoTests(p, session)) {
                // The project fails when no test runs: one class of it keeps running.
                kept.add(new String[] {mine.get(0), "its project sets failIfNoTests"});
                from = 1;
            }
            List<String> out = new ArrayList<>();
            for (String unit : mine.subList(from, mine.size())) {
                String cls = classOf.get(unit);
                Set<String> ids = wanted.get(unit);
                if (ids == null) {
                    out.add(cls);
                    continue;
                }
                for (String id : ids) {
                    int hash = id.indexOf('#');
                    String nested = hash < 0 ? "" : id.substring(0, hash);
                    String method = hash < 0 ? id : id.substring(hash + 1);
                    if (method.isEmpty()) continue;
                    out.add(cls + (nested.isEmpty() ? "" : "$" + nested) + "#" + method);
                }
            }
            if (!out.isEmpty()) lines.put(e.getKey(), out);
        }
        StringBuilder text = new StringBuilder("# written by hushrun's Maven extension from hushrun.json\n");
        for (Map.Entry<String, List<String>> e : lines.entrySet()) {
            text.append('@').append(e.getKey()).append('\n');
            for (String l : e.getValue()) text.append(l).append('\n');
        }
        File jvm = new File(dir, "test-jvm");
        jvm.mkdirs();
        new File(jvm, "events").mkdirs();
        File list = new File(jvm, "leave-out.txt");
        boolean written = false;
        if (!list.exists()) {
            Path tmp = Files.createTempFile(jvm.toPath(), ".leave-out", ".tmp");
            Files.write(tmp, text.toString().getBytes(StandardCharsets.UTF_8));
            try {
                Files.move(tmp, list.toPath(), StandardCopyOption.ATOMIC_MOVE);
                written = true;
            } catch (IOException e) {
                Files.deleteIfExists(tmp);
                errors.add("the list of tests to leave out was there already");
            }
        } else {
            errors.add("the list of tests to leave out was there already");
        }
        StringBuilder b = new StringBuilder("{\"written\":").append(written).append(",\"listed\":{");
        String sep = "";
        for (Map.Entry<String, List<String>> e : lines.entrySet()) {
            b.append(sep).append(Json.q(e.getKey())).append(':').append(strings(e.getValue()));
            sep = ",";
        }
        b.append("},\"kept\":[");
        for (int k = 0; k < kept.size(); k++) {
            if (k > 0) b.append(',');
            b.append("{\"unit\":").append(Json.q(kept.get(k)[0])).append(",\"why\":").append(Json.q(kept.get(k)[1])).append('}');
        }
        return b.append("]}").toString();
    }

    /**
     * A unit's project directory and class: {@code <dir>::<class>} names both, with {@code .} for the
     * root; a path names the file under a project's test source root that is the class's,
     * {@code pkg/Outer.java} for {@code pkg.Outer}, under the longest such root. Null for neither.
     */
    static String[] resolve(String unit, File root, Map<String, MavenProject> byDir, List<String[]> testRoots) {
        int sep = unit.lastIndexOf("::");
        if (sep >= 0) {
            String dir = unit.substring(0, sep);
            String cls = unit.substring(sep + 2);
            String base = canonical(".".equals(dir) ? root : new File(root, dir));
            if (!byDir.containsKey(base) || cls.isEmpty()) return null;
            return new String[] {base, cls};
        }
        String kind = null;
        for (String k : KINDS) if (unit.endsWith(k)) kind = k;
        if (kind == null) return null;
        String file = canonical(new File(root, unit));
        String best = null;
        String bestDir = null;
        for (String[] r : testRoots) {
            if (file.startsWith(r[0] + File.separator) && (best == null || r[0].length() > best.length())) {
                best = r[0];
                bestDir = r[1];
            }
        }
        if (best == null) return null;
        String rel = file.substring(best.length() + 1, file.length() - kind.length());
        return new String[] {bestDir, rel.replace(File.separatorChar, '.')};
    }

    private static boolean failIfNoTests(MavenProject p, MavenSession session) {
        for (String name : new String[] {"failIfNoTests", "surefire.failIfNoTests", "failsafe.failIfNoTests"}) {
            if ("true".equalsIgnoreCase(String.valueOf(property(session, name)).trim())) return true;
        }
        for (String kind : PLUGINS) {
            Plugin pl = p.getPlugin("org.apache.maven.plugins:maven-" + kind + "-plugin");
            if (pl == null) continue;
            String v = configured(pl, "failIfNoTests");
            // A value we can't read (a property left in it) counts as set.
            if (v != null && !"false".equalsIgnoreCase(v.trim())) return true;
        }
        return false;
    }

    /** A user property, else a system property, of the session. */
    private static String property(MavenSession session, String name) {
        try {
            Properties user = session.getUserProperties();
            if (user != null && user.getProperty(name) != null) return user.getProperty(name);
            Properties system = session.getSystemProperties();
            if (system != null && system.getProperty(name) != null) return system.getProperty(name);
        } catch (Throwable e) {
            // As if unset.
        }
        return null;
    }

    /**
     * Every reactor project p depends on, transitively: through dependencies of every scope, its
     * parent, its plugins' own dependencies and the annotation processors its compiler names
     * ({@code annotationProcessorPaths}, on the plugin or in any execution).
     */
    private static Set<MavenProject> closure(Map<String, MavenProject> byId, MavenProject p) {
        Set<MavenProject> seen = new LinkedHashSet<>();
        Deque<MavenProject> todo = new ArrayDeque<>();
        todo.add(p);
        while (!todo.isEmpty()) {
            MavenProject c = todo.poll();
            List<String> ids = new ArrayList<>();
            for (Dependency d : c.getDependencies()) ids.add(d.getGroupId() + ":" + d.getArtifactId());
            if (c.getParent() != null) ids.add(id(c.getParent()));
            try {
                for (Plugin pl : c.getBuildPlugins()) {
                    for (Dependency d : pl.getDependencies()) ids.add(d.getGroupId() + ":" + d.getArtifactId());
                    ids.addAll(processorPaths(pl));
                }
            } catch (Throwable e) {
                // A model without build plugins: its dependencies and parent are all it says.
            }
            for (String id : ids) {
                MavenProject r = byId.get(id);
                if (r != null && r != p && seen.add(r)) todo.add(r);
            }
        }
        return seen;
    }

    /** The {@code groupId:artifactId} of each {@code annotationProcessorPaths} entry of a plugin's configuration. */
    static List<String> processorPaths(Plugin pl) {
        List<String> out = new ArrayList<>();
        List<Object> confs = new ArrayList<>();
        confs.add(pl.getConfiguration());
        for (PluginExecution e : pl.getExecutions()) confs.add(e.getConfiguration());
        for (Object c : confs) {
            if (!(c instanceof Xpp3Dom)) continue;
            Xpp3Dom paths = ((Xpp3Dom) c).getChild("annotationProcessorPaths");
            if (paths == null) continue;
            for (Xpp3Dom path : paths.getChildren()) {
                Xpp3Dom g = path.getChild("groupId");
                Xpp3Dom a = path.getChild("artifactId");
                if (g != null && a != null && g.getValue() != null && a.getValue() != null) {
                    out.add(g.getValue().trim() + ":" + a.getValue().trim());
                }
            }
        }
        return out;
    }

    /**
     * The reactor projects whose test classes p's tests read: a test-jar dependency on them (type
     * {@code test-jar} or classifier {@code tests}, of any scope) of p or of any project p depends
     * on, to the end: a module that takes another's test-jar at compile scope hands its test
     * classes on.
     */
    private static List<String> testJars(Map<String, MavenProject> byId, MavenProject p) {
        List<String> out = new ArrayList<>();
        List<MavenProject> those = new ArrayList<>();
        those.add(p);
        those.addAll(closure(byId, p));
        for (MavenProject c : those) {
            for (Dependency d : c.getDependencies()) {
                String id = d.getGroupId() + ":" + d.getArtifactId();
                boolean tests = "test-jar".equals(d.getType()) || "tests".equals(d.getClassifier());
                if (tests && byId.containsKey(id) && !out.contains(id)) out.add(id);
            }
        }
        return out;
    }

    /** The TEST-*.xml files in a reports directory now, with their times and sizes. */
    private static String snapshot(String reports) {
        StringBuilder b = new StringBuilder("[");
        File[] files = reports == null ? null : new File(reports).listFiles();
        if (files != null) {
            String sep = "";
            List<File> sorted = new ArrayList<>();
            Collections.addAll(sorted, files);
            Collections.sort(sorted);
            for (File f : sorted) {
                String n = f.getName();
                if (!n.startsWith("TEST-") || !n.endsWith(".xml") || !f.isFile()) continue;
                b.append(sep).append("{\"name\":").append(Json.q(n)).append(",\"mtimeMs\":").append(f.lastModified())
                 .append(",\"size\":").append(f.length()).append('}');
                sep = ",";
            }
        }
        return b.append(']').toString();
    }

    private static String reportsDir(MavenProject p, Plugin pl, String kind) {
        String v = configured(pl, "reportsDirectory");
        if (v == null) return canonical(new File(p.getBuild().getDirectory(), kind + "-reports"));
        v = v.replace("${project.build.directory}", p.getBuild().getDirectory())
             .replace("${project.basedir}", p.getBasedir().getAbsolutePath())
             .replace("${basedir}", p.getBasedir().getAbsolutePath());
        if (v.contains("${")) return null;
        File f = new File(v);
        return canonical(f.isAbsolute() ? f : new File(p.getBasedir(), v));
    }

    /** The parameter as the POM sets it, on the plugin or in any execution (raw, may hold ${...}). */
    static String configured(Plugin pl, String name) {
        List<Object> confs = new ArrayList<>();
        confs.add(pl.getConfiguration());
        for (PluginExecution e : pl.getExecutions()) confs.add(e.getConfiguration());
        for (Object c : confs) {
            if (!(c instanceof Xpp3Dom)) continue;
            Xpp3Dom child = ((Xpp3Dom) c).getChild(name);
            if (child == null) continue;
            if (child.getChildCount() > 0) {
                StringBuilder s = new StringBuilder();
                for (Xpp3Dom x : child.getChildren()) s.append(s.length() > 0 ? "," : "").append(x.getValue());
                return s.toString();
            }
            return child.getValue() == null ? "" : child.getValue();
        }
        return null;
    }

    private static String roots(List<String> roots, MavenProject p) {
        List<String> out = new ArrayList<>();
        for (String r : roots) out.add(canonical(absolute(r, p)));
        return strings(out);
    }

    private static File absolute(String path, MavenProject p) {
        File f = new File(path);
        return f.isAbsolute() ? f : new File(p.getBasedir(), path);
    }

    static String canonical(File f) {
        try {
            return f.getCanonicalPath();
        } catch (IOException e) {
            return f.getAbsolutePath();
        }
    }

    private static String id(MavenProject p) {
        return p.getGroupId() + ":" + p.getArtifactId();
    }

    private static String strings(List<String> l) {
        StringBuilder s = new StringBuilder("[");
        for (int k = 0; k < l.size(); k++) s.append(k > 0 ? "," : "").append(Json.q(l.get(k)));
        return s.append(']').toString();
    }

    private static String mavenVersion() {
        try {
            Properties pr = new Properties();
            pr.load(MavenSession.class.getResourceAsStream("/META-INF/maven/org.apache.maven/maven-core/pom.properties"));
            return pr.getProperty("version");
        } catch (Throwable e) {
            return null;
        }
    }

    private static String pid() {
        try {
            return java.lang.management.ManagementFactory.getRuntimeMXBean().getName().split("@")[0];
        } catch (Throwable e) {
            return "0";
        }
    }

    /** The file, whole: a temporary name starting with '.', then a rename. */
    private static void write(File dir, String text) throws IOException {
        String name = "maven-" + pid() + "-" + System.nanoTime() + ".json";
        Path tmp = Files.createTempFile(dir.toPath(), ".maven", ".tmp");
        Files.write(tmp, text.getBytes(StandardCharsets.UTF_8));
        Files.move(tmp, new File(dir, name).toPath(), StandardCopyOption.ATOMIC_MOVE);
    }
}
