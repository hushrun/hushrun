package dev.hushrun.testjvm;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;

/**
 * The tests hushrun asks the test JVM to leave out, read once from the file that the system property
 * {@code hushrun.tests.leaveOut}, else the variable {@code HUSHRUN_TESTS_LEAVE_OUT}, names.
 *
 * <p>The file is UTF-8, one entry per line; blank lines and lines starting with {@code #} are
 * ignored, and each line is trimmed. An entry is a class's binary name ({@code Class.getName()},
 * {@code $} before a nested class's own name), alone or followed by {@code #} and a method's name:
 *
 * <ul>
 *   <li>{@code pkg.Outer} leaves out every test whose class is {@code pkg.Outer} or nested in it, at
 *       any depth ({@code pkg.Outer$Inner}, {@code pkg.Outer$Inner$Deeper});
 *   <li>{@code pkg.Outer$Inner} the same, from that nested class down;
 *   <li>{@code pkg.Outer#name} every test whose own class is exactly {@code pkg.Outer} and whose
 *       method is called {@code name}: every overload, and every invocation of a parametrized,
 *       repeated or factory method;
 *   <li>{@code pkg.Outer$Inner#name} the same, for a method of a nested class.
 * </ul>
 *
 * <p>A method's name is matched up to its first {@code [}, which no JVM method name holds (JUnit 4's
 * Parameterized names its tests {@code name[0: x=1]}). Nothing is a pattern.
 *
 * <p>When the file can't be read, nothing is left out: every test runs, and the events say why.
 */
final class LeaveOut {
    static final String PROPERTY = "hushrun.tests.leaveOut";
    static final String VARIABLE = "HUSHRUN_TESTS_LEAVE_OUT";

    private static volatile LeaveOut loaded;

    final Set<String> classes;
    final Set<String> methods;

    private LeaveOut(Set<String> classes, Set<String> methods) {
        this.classes = classes;
        this.methods = methods;
    }

    /** The list, read the first time it is asked for. Never throws. */
    static LeaveOut get() {
        LeaveOut l = loaded;
        if (l != null) return l;
        synchronized (LeaveOut.class) {
            if (loaded == null) loaded = read();
            return loaded;
        }
    }

    boolean isEmpty() {
        return classes.isEmpty() && methods.isEmpty();
    }

    /** Whether a test of {@code className}'s {@code method} (null for a class itself) is listed. */
    boolean listed(String className, String method) {
        if (className == null || className.isEmpty()) return false;
        String c = className;
        while (true) {
            if (classes.contains(c)) return true;
            int i = c.lastIndexOf('$');
            if (i <= 0) break;
            c = c.substring(0, i);
        }
        if (method == null) return false;
        return methods.contains(className + "#" + name(method));
    }

    /** A method's name up to its first '['. */
    static String name(String method) {
        int i = method.indexOf('[');
        return i < 0 ? method : method.substring(0, i);
    }

    private static LeaveOut read() {
        String path = Names.of(PROPERTY, VARIABLE);
        Set<String> classes = new HashSet<>();
        Set<String> methods = new HashSet<>();
        if (path == null) return new LeaveOut(Collections.<String>emptySet(), Collections.<String>emptySet());
        try {
            for (String raw : Files.readAllLines(Paths.get(path), StandardCharsets.UTF_8)) {
                String line = raw.trim();
                if (line.isEmpty() || line.startsWith("#")) continue;
                int hash = line.indexOf('#');
                if (hash < 0) {
                    classes.add(line);
                } else if (hash > 0 && hash < line.length() - 1) {
                    methods.add(line.substring(0, hash) + "#" + name(line.substring(hash + 1)));
                }
            }
        } catch (IOException | RuntimeException e) {
            Events.get().leaveOutUnread(path, e);
            return new LeaveOut(Collections.<String>emptySet(), Collections.<String>emptySet());
        }
        return new LeaveOut(classes, methods);
    }
}
