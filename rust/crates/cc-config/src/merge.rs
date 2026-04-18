use cc_core::CcError;
use serde_json::Value;

/// Fields whose arrays concatenate across layers instead of replacing.
/// Anything not listed here must match shapes between base and overlay.
const ARRAY_CONCAT_WHITELIST: &[&str] = &["hooks", "permissions", "additional_contexts"];

/// Coarse JSON shape classification used for mismatch detection.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Shape {
    Null,
    Object,
    Array,
    Scalar,
}

fn shape_of(v: &Value) -> Shape {
    match v {
        Value::Null => Shape::Null,
        Value::Object(_) => Shape::Object,
        Value::Array(_) => Shape::Array,
        Value::Bool(_) | Value::Number(_) | Value::String(_) => Shape::Scalar,
    }
}

/// Recursively merge `overlay` into `base`, rejecting silent type flips.
///
/// - Objects are merged field-by-field (overlay wins on same-shape conflict).
/// - Arrays replace wholesale, unless the field is in the concat whitelist
///   AND both layers agree on array shape — then values are concatenated.
/// - Scalars replace scalars of the same JSON kind.
/// - If base and overlay disagree on shape (object vs array vs scalar) for a
///   field, merge errors out naming the field path and both layer labels.
/// - `Null` on either side is treated as absence and never triggers a mismatch.
///
/// `base_label` / `overlay_label` are human-readable identifiers (typically
/// file paths) used only in error messages.
pub fn merge_json(
    base: Value,
    overlay: Value,
    base_label: &str,
    overlay_label: &str,
) -> Result<Value, CcError> {
    merge_at(base, overlay, base_label, overlay_label, "")
}

fn merge_at(
    base: Value,
    overlay: Value,
    base_label: &str,
    overlay_label: &str,
    path: &str,
) -> Result<Value, CcError> {
    // Null on either side: the other side wins without a mismatch.
    if matches!(base, Value::Null) {
        return Ok(overlay);
    }
    if matches!(overlay, Value::Null) {
        return Ok(base);
    }

    let bs = shape_of(&base);
    let os = shape_of(&overlay);

    // Same shape: recurse for objects, concat-or-replace for arrays, replace for scalars.
    if bs == os {
        return match (base, overlay) {
            (Value::Object(mut base_map), Value::Object(overlay_map)) => {
                for (key, val) in overlay_map {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    let entry = base_map.remove(&key).unwrap_or(Value::Null);
                    let merged = merge_at(entry, val, base_label, overlay_label, &child_path)?;
                    base_map.insert(key, merged);
                }
                Ok(Value::Object(base_map))
            }
            (Value::Array(mut base_arr), Value::Array(overlay_arr)) => {
                if is_concat_field(path) {
                    base_arr.extend(overlay_arr);
                    Ok(Value::Array(base_arr))
                } else {
                    Ok(Value::Array(overlay_arr))
                }
            }
            // Same-shape scalars: overlay wins.
            (_, overlay) => Ok(overlay),
        };
    }

    // Shape mismatch — refuse to silently coerce.
    Err(CcError::Config(format!(
        "field '{field}' has conflicting shapes: {bshape} in {blabel}, {oshape} in {olabel}",
        field = if path.is_empty() { "<root>" } else { path },
        bshape = shape_label_for(bs),
        blabel = base_label,
        oshape = shape_label_for(os),
        olabel = overlay_label,
    )))
}

fn shape_label_for(s: Shape) -> &'static str {
    match s {
        Shape::Null => "null",
        Shape::Object => "object",
        Shape::Array => "array",
        Shape::Scalar => "scalar",
    }
}

/// Returns true if the last dotted segment of `path` is a whitelisted
/// array-concat field. Matches top-level fields and any nested field with the
/// same leaf name (e.g. `permissions`, `extra.hooks`).
fn is_concat_field(path: &str) -> bool {
    let leaf = path.rsplit('.').next().unwrap_or(path);
    ARRAY_CONCAT_WHITELIST.contains(&leaf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn merge(base: Value, overlay: Value) -> Result<Value, CcError> {
        merge_json(base, overlay, "base.json", "overlay.json")
    }

    #[test]
    fn nested_object_merge_same_shape() {
        let base = json!({"a": 1, "b": {"x": 10, "y": 20}});
        let overlay = json!({"b": {"y": 99, "z": 30}, "c": 3});
        let result = merge(base, overlay).unwrap();
        assert_eq!(
            result,
            json!({"a": 1, "b": {"x": 10, "y": 99, "z": 30}, "c": 3})
        );
    }

    #[test]
    fn object_over_object_merges_correctly() {
        let base = json!({"a": {"x": 1}});
        let overlay = json!({"a": {"y": 2}});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"a": {"x": 1, "y": 2}}));
    }

    #[test]
    fn array_over_object_errors() {
        let base = json!({"a": {"x": 1}});
        let overlay = json!({"a": [1, 2]});
        let err = merge(base, overlay).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'a'"), "missing field name: {msg}");
        assert!(msg.contains("object"), "missing base shape: {msg}");
        assert!(msg.contains("array"), "missing overlay shape: {msg}");
        assert!(msg.contains("base.json"), "missing base label: {msg}");
        assert!(msg.contains("overlay.json"), "missing overlay label: {msg}");
    }

    #[test]
    fn scalar_over_object_errors() {
        let base = json!({"a": {"x": 1}});
        let overlay = json!({"a": "hello"});
        let err = merge(base, overlay).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'a'"));
        assert!(msg.contains("object"));
        assert!(msg.contains("scalar"));
    }

    #[test]
    fn object_over_scalar_errors() {
        let base = json!({"a": 7});
        let overlay = json!({"a": {"x": 1}});
        let err = merge(base, overlay).unwrap_err();
        assert!(err.to_string().contains("'a'"));
    }

    #[test]
    fn hooks_array_concat_preserved() {
        let base = json!({"hooks": [{"id": "h1"}]});
        let overlay = json!({"hooks": [{"id": "h2"}]});
        let result = merge(base, overlay).unwrap();
        assert_eq!(
            result,
            json!({"hooks": [{"id": "h1"}, {"id": "h2"}]})
        );
    }

    #[test]
    fn permissions_array_concat_preserved() {
        let base = json!({"permissions": ["Read"]});
        let overlay = json!({"permissions": ["Write"]});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"permissions": ["Read", "Write"]}));
    }

    #[test]
    fn non_whitelisted_array_replaces() {
        let base = json!({"items": [1, 2, 3]});
        let overlay = json!({"items": [9]});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"items": [9]}));
    }

    #[test]
    fn scalar_same_type_replaces() {
        let base = json!({"model": "x"});
        let overlay = json!({"model": "y"});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"model": "y"}));
    }

    #[test]
    fn null_overlay_keeps_base() {
        let base = json!({"a": {"x": 1}});
        let overlay = json!({"a": null});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"a": {"x": 1}}));
    }

    #[test]
    fn null_base_takes_overlay() {
        let base = json!({"a": null});
        let overlay = json!({"a": [1, 2]});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"a": [1, 2]}));
    }

    #[test]
    fn error_names_nested_field_path() {
        let base = json!({"outer": {"inner": {"x": 1}}});
        let overlay = json!({"outer": {"inner": [1, 2]}});
        let err = merge(base, overlay).unwrap_err();
        assert!(
            err.to_string().contains("'outer.inner'"),
            "expected nested path, got: {err}"
        );
    }

    #[test]
    fn nested_hooks_concat_via_leaf_match() {
        // `hooks` nested under another field should still concat by leaf match.
        let base = json!({"group": {"hooks": [1]}});
        let overlay = json!({"group": {"hooks": [2]}});
        let result = merge(base, overlay).unwrap();
        assert_eq!(result, json!({"group": {"hooks": [1, 2]}}));
    }
}
