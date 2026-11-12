package dev.hushrun.testjvm;

import java.util.ArrayList;
import java.util.List;
import org.testng.IMethodInstance;
import org.testng.IMethodInterceptor;
import org.testng.ITestContext;
import org.testng.ITestNGMethod;

/**
 * TestNG's method interceptor, which TestNG finds through {@code META-INF/services} as a listener: it
 * leaves out the test methods {@link LeaveOut} lists, by the class they run as a test of (a method
 * inherited from a base class is its subclass's) and their method. Only TestNG loads
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
                String cls = testClass(m);
                String name = t.getMethodName();
                String entry = list.entry(cls, name, false);
                if (entry == null) {
                    kept.add(m);
                } else if (dependency != null) {
                    Events.get().notLeftOut(BY, cls, name, dependency);
                    kept.add(m);
                } else {
                    Events.get().leftOut(BY, null, cls, name, entry);
                }
            }
            return kept;
        } catch (Throwable e) {
            return methods;
        }
    }

    /**
     * The class a method runs as a test of: its instance's, which for a method inherited from a
     * base class is the subclass, a test of its own. {@code getRealClass()} is the class that
     * declares it.
     */
    static String testClass(IMethodInstance m) {
        ITestNGMethod t = m.getMethod();
        try {
            if (t.getTestClass() != null && t.getTestClass().getRealClass() != null) {
                return t.getTestClass().getRealClass().getName();
            }
        } catch (RuntimeException e) {
            // An older TestNG: the instance says.
        }
        Object instance = m.getInstance();
        if (instance != null) return instance.getClass().getName();
        return t.getRealClass().getName();
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
