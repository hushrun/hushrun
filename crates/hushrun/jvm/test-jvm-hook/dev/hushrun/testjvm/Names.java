package dev.hushrun.testjvm;

/** Where a setting comes from: the JVM's system property first, then the environment. */
final class Names {
    private Names() {}

    /** The property's value, else the variable's; null when neither is set to something. */
    static String of(String property, String variable) {
        try {
            String v = System.getProperty(property);
            if (v != null && !v.trim().isEmpty()) return v.trim();
            v = System.getenv(variable);
            if (v != null && !v.trim().isEmpty()) return v.trim();
        } catch (RuntimeException e) {
            // A security manager that refuses: as if nothing were set.
        }
        return null;
    }
}
