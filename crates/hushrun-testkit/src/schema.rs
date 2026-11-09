//! A small JSON Schema validator for the subset hushrun's schemas use: `type`, `const`, `enum`,
//! `required`, `properties`, `additionalProperties: false`, `items`, `pattern`, `minimum`,
//! `$ref` to `#/$defs/…`, and `oneOf`. Tests check every JSON output against
//! `docs/schemas/` with it (plan §10.2).

use serde_json::Value;

/// Loads `docs/schemas/<name>`.
pub fn load(name: &str) -> Value {
    let path = crate::workspace_root().join("docs/schemas").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("schema is JSON")
}

/// Every violation of `schema` by `value`, as `path: message`.
pub fn violations(schema: &Value, value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    check(schema, schema, value, "$", &mut out);
    out
}

/// Panics with every violation.
pub fn assert_valid(schema: &Value, value: &Value) {
    let found = violations(schema, value);
    assert!(
        found.is_empty(),
        "output does not match {}:\n{}\n{value:#}",
        schema["title"],
        found.join("\n")
    );
}

fn check(root: &Value, schema: &Value, value: &Value, path: &str, out: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if let Some(pointer) = reference.strip_prefix("#/") {
            let target = pointer.split('/').fold(root, |node, part| &node[part]);
            if target.is_null() {
                out.push(format!("{path}: unresolved $ref {reference}"));
                return;
            }
            check(root, target, value, path, out);
        } else {
            // Another schema file next to this one.
            let other = load(reference);
            check(&other, &other, value, path, out);
        }
    }
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let matching = options
            .iter()
            .filter(|option| {
                let mut found = Vec::new();
                check(root, option, value, path, &mut found);
                found.is_empty()
            })
            .count();
        if matching != 1 {
            out.push(format!(
                "{path}: matches {matching} of the oneOf options, not exactly 1"
            ));
        }
    }
    if let Some(expected) = schema.get("const")
        && value != expected
    {
        out.push(format!("{path}: {value} is not {expected}"));
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        out.push(format!("{path}: {value} is not one of {options:?}"));
    }
    if let Some(kind) = schema.get("type") {
        let kinds: Vec<&str> = match kind {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !kinds.iter().any(|k| type_matches(k, value)) {
            out.push(format!("{path}: {value} is not of type {kinds:?}"));
            return;
        }
    }
    if let (Some(pattern), Some(text)) = (
        schema.get("pattern").and_then(Value::as_str),
        value.as_str(),
    ) {
        let regex = regex_automata::meta::Regex::new(pattern).expect("valid pattern");
        if !regex.is_match(text) {
            out.push(format!("{path}: {text:?} does not match {pattern}"));
        }
    }
    if let (Some(minimum), Some(number)) = (
        schema.get("minimum").and_then(Value::as_f64),
        value.as_f64(),
    ) && number < minimum
    {
        out.push(format!("{path}: {number} is below {minimum}"));
    }
    if let (Some(maximum), Some(number)) = (
        schema.get("maximum").and_then(Value::as_f64),
        value.as_f64(),
    ) && number > maximum
    {
        out.push(format!("{path}: {number} is above {maximum}"));
    }
    if let (Some(min), Some(text)) = (
        schema.get("minLength").and_then(Value::as_u64),
        value.as_str(),
    ) && (text.chars().count() as u64) < min
    {
        out.push(format!("{path}: {text:?} is shorter than {min}"));
    }
    if let Value::Object(object) = value {
        if let Some(names) = schema.get("propertyNames") {
            for key in object.keys() {
                check(
                    root,
                    names,
                    &Value::String(key.clone()),
                    &format!("{path}.{key} (name)"),
                    out,
                );
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let key = required.as_str().expect("required names are strings");
            if !object.contains_key(key) {
                out.push(format!("{path}: missing {key}"));
            }
        }
        for (key, child) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(child_schema) => {
                    check(root, child_schema, child, &format!("{path}.{key}"), out)
                }
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => out.push(format!("{path}: unexpected {key}")),
                    Some(extra @ Value::Object(_)) => {
                        check(root, extra, child, &format!("{path}.{key}"), out);
                    }
                    _ => {}
                },
            }
        }
    }
    if let (Some(items), Value::Array(array)) = (schema.get("items"), value) {
        for (i, item) in array.iter().enumerate() {
            check(root, items, item, &format!("{path}[{i}]"), out);
        }
    }
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_violations() {
        let schema = json!({
            "type": "object",
            "required": ["a"],
            "additionalProperties": false,
            "properties": {
                "a": {"type": "string", "pattern": "^x"},
                "b": {"type": "array", "items": {"enum": [1, 2]}},
            },
        });
        assert!(violations(&schema, &json!({"a": "xy", "b": [1, 2]})).is_empty());
        let found = violations(&schema, &json!({"a": "y", "b": [3], "c": true}));
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(violations(&schema, &json!({})), ["$: missing a"]);
    }
}
