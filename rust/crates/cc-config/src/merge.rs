use serde_json::Value;

/// Recursively merge `overlay` into `base`.
/// - Objects are merged field-by-field (overlay wins on conflict).
/// - All other types: overlay replaces base entirely.
pub fn merge_json(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut base_map), Value::Object(overlay_map)) => {
            for (key, val) in overlay_map {
                let entry = base_map.remove(&key).unwrap_or(Value::Null);
                base_map.insert(key, merge_json(entry, val));
            }
            Value::Object(base_map)
        }
        // Overlay wins for all non-object types (including Null overlay clears a key).
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
        assert_eq!(result, json!({"a": 1, "b": {"x": 10, "y": 99, "z": 30}, "c": 3}));
    }
}
