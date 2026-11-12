package dev.hushrun.maven;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Just enough JSON: reads hushrun.json into maps, lists, strings, numbers and booleans; writes strings. */
final class Json {
    private final String s;
    private int i;

    private Json(String s) {
        this.s = s;
    }

    /** The value {@code text} holds; throws {@link IllegalArgumentException} when it isn't JSON. */
    static Object parse(String text) {
        Json j = new Json(text);
        j.space();
        Object v = j.value();
        j.space();
        if (j.i != j.s.length()) throw j.bad("trailing text");
        return v;
    }

    private Object value() {
        if (i >= s.length()) throw bad("end of text");
        char c = s.charAt(i);
        if (c == '{') return object();
        if (c == '[') return array();
        if (c == '"') return string();
        if (s.startsWith("true", i)) { i += 4; return Boolean.TRUE; }
        if (s.startsWith("false", i)) { i += 5; return Boolean.FALSE; }
        if (s.startsWith("null", i)) { i += 4; return null; }
        int start = i;
        while (i < s.length() && "+-0123456789.eE".indexOf(s.charAt(i)) >= 0) i++;
        if (start == i) throw bad("a value");
        return Double.valueOf(s.substring(start, i));
    }

    private Map<String, Object> object() {
        Map<String, Object> m = new LinkedHashMap<>();
        i++;
        space();
        if (peek() == '}') { i++; return m; }
        while (true) {
            space();
            if (peek() != '"') throw bad("a name");
            String k = string();
            space();
            if (peek() != ':') throw bad("':'");
            i++;
            space();
            m.put(k, value());
            space();
            char c = peek();
            i++;
            if (c == '}') return m;
            if (c != ',') throw bad("',' or '}'");
        }
    }

    private List<Object> array() {
        List<Object> l = new ArrayList<>();
        i++;
        space();
        if (peek() == ']') { i++; return l; }
        while (true) {
            space();
            l.add(value());
            space();
            char c = peek();
            i++;
            if (c == ']') return l;
            if (c != ',') throw bad("',' or ']'");
        }
    }

    private String string() {
        StringBuilder b = new StringBuilder();
        i++;
        while (true) {
            if (i >= s.length()) throw bad("end of a string");
            char c = s.charAt(i++);
            if (c == '"') return b.toString();
            if (c != '\\') { b.append(c); continue; }
            if (i >= s.length()) throw bad("an escape");
            char e = s.charAt(i++);
            switch (e) {
                case '"': b.append('"'); break;
                case '\\': b.append('\\'); break;
                case '/': b.append('/'); break;
                case 'b': b.append('\b'); break;
                case 'f': b.append('\f'); break;
                case 'n': b.append('\n'); break;
                case 'r': b.append('\r'); break;
                case 't': b.append('\t'); break;
                case 'u':
                    if (i + 4 > s.length()) throw bad("four hex digits");
                    b.append((char) Integer.parseInt(s.substring(i, i + 4), 16));
                    i += 4;
                    break;
                default: throw bad("an escape");
            }
        }
    }

    private char peek() {
        if (i >= s.length()) throw bad("end of text");
        return s.charAt(i);
    }

    private void space() {
        while (i < s.length() && " \t\r\n".indexOf(s.charAt(i)) >= 0) i++;
    }

    private IllegalArgumentException bad(String expected) {
        return new IllegalArgumentException("not JSON: expected " + expected + " at " + i);
    }

    /** {@code text} as a JSON string. */
    static String q(String text) {
        if (text == null) return "null";
        StringBuilder b = new StringBuilder(text.length() + 2).append('"');
        for (int k = 0; k < text.length(); k++) {
            char c = text.charAt(k);
            if (c == '"' || c == '\\') b.append('\\').append(c);
            else if (c < 0x20 || c == 0x2028 || c == 0x2029) b.append(String.format(java.util.Locale.ROOT, "\\u%04x", (int) c));
            else b.append(c);
        }
        return b.append('"').toString();
    }
}
