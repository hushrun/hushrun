package dev.hushrun.testjvm;

import java.io.File;

/**
 * Where a setting comes from: the JVM's system property first, then a variable of the environment,
 * then a path below the directory {@code HUSHRUN_UNITS} names, hushrun's directory for the run.
 */
final class Names {
    static final String UNITS = "HUSHRUN_UNITS";

    private Names() {}

    /** The property's value, else the variable's, else {@code $HUSHRUN_UNITS/test-jvm/<child>}; null for none. */
    static String of(String property, String variable, String child) {
        try {
            String v = System.getProperty(property);
            if (v != null && !v.trim().isEmpty()) return v.trim();
            v = System.getenv(variable);
            if (v != null && !v.trim().isEmpty()) return v.trim();
            v = System.getenv(UNITS);
            if (v != null && !v.trim().isEmpty()) return new File(new File(v.trim(), "test-jvm"), child).getPath();
        } catch (RuntimeException e) {
            // A security manager that refuses: as if nothing were set.
        }
        return null;
    }

    /** This JVM's directory: the system property {@code basedir}, else its working directory, canonical. */
    static String jvmDir() {
        String d = null;
        try {
            d = System.getProperty("basedir");
            if (d == null || d.trim().isEmpty()) d = System.getProperty("user.dir");
            return d == null ? null : new File(d.trim()).getCanonicalPath();
        } catch (Exception e) {
            return d;
        }
    }

    /** {@code path}, canonical, or as it is when it can't be made so. */
    static String canonical(String path) {
        try {
            return new File(path).getCanonicalPath();
        } catch (Exception e) {
            return path;
        }
    }
}
