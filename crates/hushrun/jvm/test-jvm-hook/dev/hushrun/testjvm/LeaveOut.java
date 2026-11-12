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
 * {@code hushrun.tests.leaveOut} names, else the variable {@code HUSHRUN_TESTS_LEAVE_OUT}, else
 * {@code $HUSHRUN_UNITS/test-jvm/leave-out.txt} when {@code HUSHRUN_UNITS} is set.
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
 * <p>A line {@code @<directory>} scopes the entries after it, up to the next such line, to a JVM whose
 * directory is that one: its system property {@code basedir}, else its working directory, both
 * compared as canonical paths. Entries before any such line apply in every JVM.
 *
 * <p>A method's name is matched whole, as the test's source names it: a Spock feature, a Kotlin
 * name in backticks or a parametrized display name may hold {@code [}, spaces or anything else.
 * One exception, for the JUnit Vintage engine only: JUnit 4's Parameterized names each invocation
 * {@code name[0: x=1]}, which an entry {@code Class#name} matches. Nothing is a pattern.
 *
 * <p>When there is no such file, nothing is left out. When it can't be read, nothing is left out
 * either, and the events say why.
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

    /**
     * The entry that lists a test of {@code className}'s {@code method} (null for a class itself),
     * as the list spells it; null when none does. {@code vintage}: the test is the JUnit Vintage
     * engine's, whose Parameterized invocations are {@code name[...]}.
     */
    String entry(String className, String method, boolean vintage) {
        if (className == null || className.isEmpty()) return null;
        String c = className;
        while (true) {
            if (classes.contains(c)) return c;
            int i = c.lastIndexOf('$');
            if (i <= 0) break;
            c = c.substring(0, i);
        }
        if (method == null) return null;
        String whole = className + "#" + method;
        if (methods.contains(whole)) return whole;
        int bracket = method.indexOf('[');
        if (vintage && bracket > 0 && method.endsWith("]")) {
            String invoked = className + "#" + method.substring(0, bracket);
            if (methods.contains(invoked)) return invoked;
        }
        return null;
    }

    private static LeaveOut read() {
        String path = Names.of(PROPERTY, VARIABLE, "leave-out.txt");
        Set<String> classes = new HashSet<>();
        Set<String> methods = new HashSet<>();
        if (path == null || !Files.exists(Paths.get(path))) {
            return new LeaveOut(Collections.<String>emptySet(), Collections.<String>emptySet());
        }
        try {
            String here = null;
            boolean applies = true;
            for (String raw : Files.readAllLines(Paths.get(path), StandardCharsets.UTF_8)) {
                String line = raw.trim();
                if (line.isEmpty() || line.startsWith("#")) continue;
                if (line.startsWith("@")) {
                    if (here == null) here = Names.jvmDir();
                    String scope = line.substring(1).trim();
                    applies = here != null && !scope.isEmpty() && Names.canonical(scope).equals(here);
                    continue;
                }
                if (!applies) continue;
                int hash = line.indexOf('#');
                if (hash < 0) {
                    classes.add(line);
                } else if (hash > 0 && hash < line.length() - 1) {
                    methods.add(line);
                }
            }
        } catch (IOException | RuntimeException e) {
            Events.get().leaveOutUnread(path, e);
            return new LeaveOut(Collections.<String>emptySet(), Collections.<String>emptySet());
        }
        return new LeaveOut(classes, methods);
    }
}
