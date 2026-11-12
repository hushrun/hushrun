package dev.hushrun.testjvm;

import java.util.ArrayList;
import java.util.List;
import org.testng.IMethodInstance;
import org.testng.IMethodInterceptor;
import org.testng.ITestContext;
import org.testng.ITestNGMethod;

/**
 * TestNG's method interceptor, which TestNG finds through {@code META-INF/services} as a listener: it
 * leaves out the test methods {@link LeaveOut} lists, by their class and method. Only TestNG loads
 * this class, so where TestNG isn't on the classpath nothing refers to a TestNG type.
 *
 * <p>When a method of the test context depends on other methods or groups, it leaves nothing of that
 * context out: a dependency left out would skip the methods that depend on it, and a skipped test
 * reads as passed. Any error leaves every method in.
 */
public final class TestNGInterceptor implements IMethodInterceptor {
    private static final String BY = "testng";

    @Override
    public List<IMethodInstance> intercept(List<IMethodInstance> methods, ITestContext context) {
        try {
            LeaveOut list = LeaveOut.get();
            if (list.isEmpty()) return methods;
            String dependency = dependency(methods, context);
            List<IMethodInstance> kept = new ArrayList<>(methods.size());
            for (IMethodInstance m : methods) {
                ITestNGMethod t = m.getMethod();
                String cls = t.getRealClass().getName();
                String name = t.getMethodName();
                if (!list.listed(cls, name)) {
                    kept.add(m);
                } else if (dependency != null) {
                    Events.get().notLeftOut(BY, cls, name, dependency);
                    kept.add(m);
                } else {
                    Events.get().leftOut(BY, cls, name);
                }
            }
            return kept;
        } catch (Throwable e) {
            return methods;
        }
    }

    /** Why nothing of the context may be left out, or null. */
    private static String dependency(List<IMethodInstance> methods, ITestContext context) {
        List<ITestNGMethod> all = new ArrayList<>();
        if (context != null && context.getAllTestMethods() != null) {
            for (ITestNGMethod t : context.getAllTestMethods()) all.add(t);
        }
        for (IMethodInstance m : methods) all.add(m.getMethod());
        for (ITestNGMethod t : all) {
            String[] onMethods = t.getMethodsDependedUpon();
            String[] onGroups = t.getGroupsDependedUpon();
            if ((onMethods != null && onMethods.length > 0) || (onGroups != null && onGroups.length > 0)) {
                return t.getRealClass().getName() + "#" + t.getMethodName() + " depends on other tests";
            }
        }
        return null;
    }
}
