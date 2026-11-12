package dev.hushrun.testjvm;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.lang.management.ManagementFactory;
import java.nio.charset.StandardCharsets;
import java.util.Locale;

/**
 * What the test JVM did, one JSON object a line, in a file of its own in the directory that the
 * system property {@code hushrun.tests.events} names, else the variable {@code HUSHRUN_TESTS_EVENTS},
 * else {@code $HUSHRUN_UNITS/test-jvm/events}: {@code <pid>-<nanos>.jsonl}, created (with the
 * directory) on the first event, each line flushed as it is written,
 * so a JVM that dies leaves every line before it. Nothing is written when neither is set, and an
 * error writing stops the writing, never the tests.
 *
 * <p>The lines, by their {@code "e"}:
 *
 * <ul>
 *   <li>{@code jvm}: first, once: {@code pid}, {@code dir} (the JVM's working directory) and
 *       {@code basedir} (the system property of that name, or null);
 *   <li>{@code leave-out-unread}: the list couldn't be read ({@code path}, {@code error}), and
 *       nothing is left out;
 *   <li>{@code left-out}: {@code by} ({@code junit-platform} or {@code testng}) left out a listed
 *       {@code class}'s {@code method} (null when it left out a class with no method under it);
 *   <li>{@code not-left-out}: {@code by} kept a listed {@code class}'s {@code method}, for a
 *       {@code reason};
 *   <li>{@code plan-started}, {@code plan-finished}: the JUnit Platform began and ended a test plan;
 *   <li>{@code started}, {@code skipped} (with {@code reason}), {@code finished} (with {@code status}:
 *       {@code SUCCESSFUL}, {@code FAILED} or {@code ABORTED}, {@code ms} and {@code message}): a test
 *       or container of the JUnit Platform, with {@code test} (whether it is a test), {@code uid},
 *       {@code class}, {@code method} and {@code params} from its source (null when it has none).
 * </ul>
 */
final class Events {
    static final String PROPERTY = "hushrun.tests.events";
    static final String VARIABLE = "HUSHRUN_TESTS_EVENTS";

    private static final Events INSTANCE = new Events();

    private OutputStream out;
    private boolean opened;

    static Events get() {
        return INSTANCE;
    }

    void leaveOutUnread(String path, Throwable error) {
        write("{\"e\":\"leave-out-unread\",\"path\":" + q(path) + ",\"error\":" + q(String.valueOf(error)) + "}");
    }

    void leftOut(String by, String className, String method) {
        write("{\"e\":\"left-out\",\"by\":" + q(by) + ",\"class\":" + q(className) + ",\"method\":" + q(method) + "}");
    }

    void notLeftOut(String by, String className, String method, String reason) {
        write("{\"e\":\"not-left-out\",\"by\":" + q(by) + ",\"class\":" + q(className) + ",\"method\":" + q(method)
            + ",\"reason\":" + q(reason) + "}");
    }

    void plan(boolean started) {
        write(started ? "{\"e\":\"plan-started\"}" : "{\"e\":\"plan-finished\"}");
    }

    void started(boolean test, String uid, String className, String method, String params) {
        write("{\"e\":\"started\"," + ident(test, uid, className, method, params) + "}");
    }

    void skipped(boolean test, String uid, String className, String method, String params, String reason) {
        write("{\"e\":\"skipped\"," + ident(test, uid, className, method, params) + ",\"reason\":" + q(reason) + "}");
    }

    void finished(boolean test, String uid, String className, String method, String params, String status, long ms,
                  String message) {
        write("{\"e\":\"finished\"," + ident(test, uid, className, method, params) + ",\"status\":" + q(status)
            + ",\"ms\":" + ms + ",\"message\":" + q(message) + "}");
    }

    private static String ident(boolean test, String uid, String className, String method, String params) {
        return "\"test\":" + test + ",\"uid\":" + q(uid) + ",\"class\":" + q(className) + ",\"method\":" + q(method)
            + ",\"params\":" + q(params);
    }

    private synchronized void write(String line) {
        try {
            if (!opened) {
                opened = true;
                open();
            }
            if (out == null) return;
            out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
            out.flush();
        } catch (Throwable e) {
            close();
        }
    }

    private void open() throws IOException {
        String dir = Names.of(PROPERTY, VARIABLE, "events");
        if (dir == null) return;
        new File(dir).mkdirs();
        String pid = ManagementFactory.getRuntimeMXBean().getName().split("@")[0];
        File file = null;
        for (int i = 0; i < 100; i++) {
            File f = new File(dir, pid + "-" + System.nanoTime() + (i == 0 ? "" : "-" + i) + ".jsonl");
            if (f.createNewFile()) {
                file = f;
                break;
            }
        }
        if (file == null) return;
        out = new FileOutputStream(file);
        String line = "{\"e\":\"jvm\",\"pid\":" + q(pid) + ",\"dir\":" + q(System.getProperty("user.dir"))
            + ",\"basedir\":" + q(System.getProperty("basedir")) + "}\n";
        out.write(line.getBytes(StandardCharsets.UTF_8));
        out.flush();
    }

    private void close() {
        try {
            if (out != null) out.close();
        } catch (Throwable ignored) {
            // Nothing more to write anyway.
        }
        out = null;
    }

    static String q(String s) {
        if (s == null) return "null";
        StringBuilder b = new StringBuilder(s.length() + 2).append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '"' || c == '\\') b.append('\\').append(c);
            else if (c < 0x20 || c == 0x2028 || c == 0x2029) b.append(String.format(Locale.ROOT, "\\u%04x", (int) c));
            else b.append(c);
        }
        return b.append('"').toString();
    }
}
