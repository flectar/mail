//! Three-way merges of record bodies: a field changed on one side only keeps
//! that change; a field both sides changed goes to the newer change.

use serde_json::{Map, Value};

/// Merge `local` and `remote`, both changed from `base`. Objects merge field
/// by field, recursively; anything else is taken whole from the side that
/// changed it, or from `remote` when `remote_newer` and both did.
pub fn merge(base: Option<&Value>, local: &Value, remote: &Value, remote_newer: bool) -> Value {
    if local == remote {
        return local.clone();
    }
    match base {
        Some(base) if base == local => return remote.clone(),
        Some(base) if base == remote => return local.clone(),
        _ => {}
    }
    match (base, local, remote) {
        (Some(Value::Object(base)), Value::Object(local), Value::Object(remote)) => {
            Value::Object(merge_objects(Some(base), local, remote, remote_newer))
        }
        (None, Value::Object(local), Value::Object(remote)) => {
            Value::Object(merge_objects(None, local, remote, remote_newer))
        }
        _ if remote_newer => remote.clone(),
        _ => local.clone(),
    }
}

fn merge_objects(
    base: Option<&Map<String, Value>>,
    local: &Map<String, Value>,
    remote: &Map<String, Value>,
    remote_newer: bool,
) -> Map<String, Value> {
    let mut keys: Vec<&String> = local.keys().chain(remote.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut merged = Map::new();
    for key in keys {
        let base_value = base.and_then(|base| base.get(key));
        let value = match (local.get(key), remote.get(key)) {
            (Some(local), Some(remote)) => Some(merge(base_value, local, remote, remote_newer)),
            // Only one side has the field: it either added it, or the other
            // side removed it.
            (Some(only), None) | (None, Some(only)) => {
                let added = base_value.is_none();
                let unchanged = base_value == Some(only);
                if added {
                    Some(only.clone())
                } else if unchanged {
                    None
                } else if remote_newer == remote.contains_key(key) {
                    Some(only.clone())
                } else {
                    None
                }
            }
            (None, None) => None,
        };
        if let Some(value) = value {
            merged.insert(key.clone(), value);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn changes_to_different_fields_both_survive() {
        let base = json!({"name": "Work", "color": "blue"});
        let local = json!({"name": "Office", "color": "blue"});
        let remote = json!({"name": "Work", "color": "green"});
        assert_eq!(
            merge(Some(&base), &local, &remote, false),
            json!({"name": "Office", "color": "green"})
        );
    }

    #[test]
    fn the_newer_side_wins_a_field_both_changed() {
        let base = json!({"name": "Work"});
        let local = json!({"name": "Office"});
        let remote = json!({"name": "Job"});
        assert_eq!(merge(Some(&base), &local, &remote, true), remote);
        assert_eq!(merge(Some(&base), &local, &remote, false), local);
    }

    #[test]
    fn added_and_removed_fields_follow_the_side_that_changed_them() {
        let base = json!({"a": 1, "b": 2});
        let local = json!({"a": 1, "b": 2, "c": 3});
        let remote = json!({"a": 1});
        assert_eq!(
            merge(Some(&base), &local, &remote, false),
            json!({"a": 1, "c": 3})
        );
    }

    #[test]
    fn nested_objects_merge_and_values_replace_whole() {
        let base = json!({"connection": {"host": "a", "port": 1}, "list": [1]});
        let local = json!({"connection": {"host": "b", "port": 1}, "list": [1, 2]});
        let remote = json!({"connection": {"host": "a", "port": 2}, "list": [1]});
        assert_eq!(
            merge(Some(&base), &local, &remote, true),
            json!({"connection": {"host": "b", "port": 2}, "list": [1, 2]})
        );
    }

    #[test]
    fn without_a_base_the_newer_side_wins_conflicts() {
        let local = json!({"a": 1, "b": 1});
        let remote = json!({"a": 2, "c": 3});
        assert_eq!(
            merge(None, &local, &remote, true),
            json!({"a": 2, "b": 1, "c": 3})
        );
    }
}
