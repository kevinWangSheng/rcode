use serde_json::Value;

/// Recursively merge `overlay` into `base`.
/// - Objects are merged field-by-field (overlay wins on conflict).
/// - All other types (arrays, primitives): overlay replaces base entirely.
///
/// This matches TS Claude Code's settings merge behavior: permission arrays
/// like `allow`/`deny` are fully replaced by higher-priority sources, not unioned.
pub fn merge_json(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut base_map), Value::Object(overlay_map)) => {
            for (key, val) in overlay_map {
                let entry = base_map.remove(&key).unwrap_or(Value::Null);
                base_map.insert(key, merge_json(entry, val));
            }
            Value::Object(base_map)
        }
        // Overlay wins for arrays and primitives.
        (_, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_merge() {
        let base = json!({"a": 1, "b": {"x": 10, "y": 20}});
        let overlay = json!({"b": {"y": 99, "z": 30}, "c": 3});
        let result = merge_json(base, overlay);
        assert_eq!(
            result,
            json!({"a": 1, "b": {"x": 10, "y": 99, "z": 30}, "c": 3})
        );
    }

    #[test]
    fn array_overlay_replaces() {
        let base = json!({"tags": ["a", "b"]});
        let overlay = json!({"tags": ["c"]});
        let result = merge_json(base, overlay);
        assert_eq!(result, json!({"tags": ["c"]}));
    }

    #[test]
    fn primitive_overlay_wins() {
        let base = json!({"x": 1});
        let overlay = json!({"x": 2});
        let result = merge_json(base, overlay);
        assert_eq!(result, json!({"x": 2}));
    }

    #[test]
    fn missing_key_in_overlay_preserved_from_base() {
        let base = json!({"a": 1, "b": 2});
        let overlay = json!({"b": 3});
        let result = merge_json(base, overlay);
        assert_eq!(result, json!({"a": 1, "b": 3}));
    }
}
