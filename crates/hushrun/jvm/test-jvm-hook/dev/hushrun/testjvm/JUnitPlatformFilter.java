package dev.hushrun.testjvm;

import java.util.Optional;
import org.junit.platform.engine.FilterResult;
import org.junit.platform.engine.TestDescriptor;
import org.junit.platform.engine.TestSource;
import org.junit.platform.engine.support.descriptor.ClassSource;
import org.junit.platform.engine.support.descriptor.MethodSource;
import org.junit.platform.launcher.PostDiscoveryFilter;

/**
 * The JUnit Platform's filter after discovery, which its launcher finds through
 * {@code META-INF/services} (JUnit Platform 1.7 and later): it leaves out the tests {@link LeaveOut}
 * lists, by their source's class and method, and nothing else. Each test it leaves out is told
 * with the entry that listed it.
 *
 * <p>The launcher asks it of every descriptor with no children: a test, a parametrized method before
 * its invocations exist, a class with no tests. TestNG's engine is left to {@link TestNGInterceptor},
 * which runs inside TestNG wherever it runs and knows what depends on what. Any error leaves the
 * descriptor in.
 */
public final class JUnitPlatformFilter implements PostDiscoveryFilter {
    private static final String BY = "junit-platform";

    @Override
    public FilterResult apply(TestDescriptor d) {
        try {
            LeaveOut list = LeaveOut.get();
            if (list.isEmpty()) return FilterResult.included(null);
            if (d.getUniqueId().toString().startsWith("[engine:testng]")) return FilterResult.included(null);
            Optional<TestSource> s = d.getSource();
            if (!s.isPresent()) return FilterResult.included(null);
            String cls;
            String method = null;
            if (s.get() instanceof MethodSource) {
                MethodSource m = (MethodSource) s.get();
                cls = m.getClassName();
                method = m.getMethodName();
            } else if (s.get() instanceof ClassSource) {
                cls = ((ClassSource) s.get()).getClassName();
            } else {
                return FilterResult.included(null);
            }
            boolean vintage = d.getUniqueId().toString().startsWith("[engine:junit-vintage]");
            String entry = list.entry(cls, method, vintage);
            if (entry == null) return FilterResult.included(null);
            Events.get().leftOut(BY, d.getUniqueId().toString(), cls, method, entry);
            return FilterResult.excluded("hushrun: already passed on the files it read");
        } catch (Throwable e) {
            return FilterResult.included(null);
        }
    }
}
