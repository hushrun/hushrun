package dev.hushrun.testjvm;

import java.util.Map;
import java.util.Optional;
import java.util.concurrent.ConcurrentHashMap;
import org.junit.platform.engine.TestExecutionResult;
import org.junit.platform.engine.TestSource;
import org.junit.platform.engine.support.descriptor.ClassSource;
import org.junit.platform.engine.support.descriptor.MethodSource;
import org.junit.platform.launcher.TestExecutionListener;
import org.junit.platform.launcher.TestIdentifier;
import org.junit.platform.launcher.TestPlan;

/**
 * The JUnit Platform's listener, which its launcher finds through {@code META-INF/services}: it
 * writes each test's and container's start, skip and end to {@link Events}. It decides nothing, and
 * any error is dropped.
 */
public final class JUnitPlatformListener implements TestExecutionListener {
    private final Map<String, Long> started = new ConcurrentHashMap<>();

    @Override
    public void testPlanExecutionStarted(TestPlan plan) {
        try {
            Events.get().plan(true);
        } catch (Throwable ignored) {
            // Never the tests' problem.
        }
    }

    @Override
    public void testPlanExecutionFinished(TestPlan plan) {
        try {
            Events.get().plan(false);
        } catch (Throwable ignored) {
            // Never the tests' problem.
        }
    }

    @Override
    public void executionStarted(TestIdentifier id) {
        try {
            started.put(id.getUniqueId(), System.nanoTime());
            String[] s = source(id);
            Events.get().started(id.isTest(), id.getUniqueId(), s[0], s[1], s[2]);
        } catch (Throwable ignored) {
            // Never the tests' problem.
        }
    }

    @Override
    public void executionSkipped(TestIdentifier id, String reason) {
        try {
            String[] s = source(id);
            Events.get().skipped(id.isTest(), id.getUniqueId(), s[0], s[1], s[2], reason);
        } catch (Throwable ignored) {
            // Never the tests' problem.
        }
    }

    @Override
    public void executionFinished(TestIdentifier id, TestExecutionResult result) {
        try {
            Long t0 = started.remove(id.getUniqueId());
            long ms = t0 == null ? 0 : (System.nanoTime() - t0) / 1000000L;
            String message = null;
            Optional<Throwable> t = result.getThrowable();
            if (t.isPresent()) message = t.get().getClass().getName() + ": " + t.get().getMessage();
            String[] s = source(id);
            Events.get().finished(id.isTest(), id.getUniqueId(), s[0], s[1], s[2], result.getStatus().name(), ms, message);
        } catch (Throwable ignored) {
            // Never the tests' problem.
        }
    }

    /** Its source's class, method and parameter types, each null when it has none. */
    private static String[] source(TestIdentifier id) {
        Optional<TestSource> s = id.getSource();
        if (s.isPresent() && s.get() instanceof MethodSource) {
            MethodSource m = (MethodSource) s.get();
            return new String[] {m.getClassName(), m.getMethodName(), m.getMethodParameterTypes()};
        }
        if (s.isPresent() && s.get() instanceof ClassSource) {
            return new String[] {((ClassSource) s.get()).getClassName(), null, null};
        }
        return new String[] {null, null, null};
    }
}
