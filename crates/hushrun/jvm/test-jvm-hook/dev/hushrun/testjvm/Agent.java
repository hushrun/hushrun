package dev.hushrun.testjvm;

import java.util.Map;

/**
 * The hook as a Java agent ({@code -javaagent:<this jar>}), which Gradle's script gives each test
 * JVM of a run: it writes that the JVM began, and, from a shutdown hook, that it ended and whether
 * a test ended it ({@link Events}). A JVM that began and never said it ended was halted or killed;
 * one that ended because a test called {@code System.exit} didn't run what came after. Both tell
 * what no test runner's events say where no listener of the hook loads (JUnit 4 and TestNG on
 * Gradle's own runners). It transforms no class, and any error is dropped.
 */
public final class Agent {
    /** Frames of a test or its framework, below an exit: a test ended the JVM. */
    private static final String[] TEST_FRAMES = {
        "org.junit.", "junit.", "org.testng.", "org.spockframework.", "spock.", "io.kotest.",
        "org.gradle.api.internal.tasks.testing.junit", "org.gradle.api.internal.tasks.testing.testng"
    };

    private Agent() {}

    public static void premain(String args) {
        try {
            Events.get().began();
            Runtime.getRuntime().addShutdownHook(new Thread(Agent::ended, "hushrun-test-jvm-end"));
        } catch (Throwable ignored) {
            // Never the tests' problem.
        }
    }

    private static void ended() {
        try {
            String exit = null;
            boolean byTest = false;
            for (Map.Entry<Thread, StackTraceElement[]> e : Thread.getAllStackTraces().entrySet()) {
                if (e.getKey() == Thread.currentThread()) continue;
                StackTraceElement[] stack = e.getValue();
                int at = -1;
                for (int i = 0; i < stack.length; i++) {
                    String c = stack[i].getClassName();
                    if ((c.equals("java.lang.Runtime") || c.equals("java.lang.System") || c.equals("java.lang.Shutdown"))
                        && stack[i].getMethodName().equals("exit")) {
                        at = i;
                    }
                }
                if (at < 0) continue;
                StringBuilder b = new StringBuilder();
                for (int i = at + 1; i < stack.length && i < at + 40; i++) {
                    String c = stack[i].getClassName();
                    for (String t : TEST_FRAMES) {
                        if (c.startsWith(t)) byTest = true;
                    }
                    if (b.length() < 600) b.append(b.length() == 0 ? "" : " ").append(c).append('.').append(stack[i].getMethodName());
                }
                exit = b.toString();
            }
            Events.get().ended(exit != null, byTest, exit);
        } catch (Throwable ignored) {
            // Nothing more to write anyway.
        }
    }
}
