use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, BTreeSet};

pub type UserMetadata = BTreeMap<String, Value>;

const RESERVED_PREFIXES: [&str; 2] = ["ridal_", "meta_"];

#[derive(Debug, Clone, PartialEq)]
pub enum FlattenedMetadataValue {
    String(String),
    I64(i64),
    F64(f64),
    U8(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedMetadataAttribute {
    pub name: String,
    pub value: FlattenedMetadataValue,
}

#[allow(dead_code)]
pub fn value_to_metadata(value: Value) -> Result<UserMetadata, String> {
    match value {
        Value::Object(map) => {
            let metadata: UserMetadata = map.into_iter().collect();
            validate_metadata(&metadata)?;
            Ok(metadata)
        }
        _ => Err("metadata must be a mapping/object".to_string()),
    }
}

pub fn parse_cli_metadata(entries: &[String]) -> Result<UserMetadata, String> {
    let mut metadata = UserMetadata::new();
    for entry in entries {
        let (key, value) = parse_cli_metadata_entry(entry)?;
        if metadata.contains_key(&key) {
            return Err(format!("Duplicate metadata key: {key}"));
        }
        metadata.insert(key, value);
    }
    validate_metadata(&metadata)?;
    Ok(metadata)
}

fn parse_cli_metadata_entry(entry: &str) -> Result<(String, Value), String> {
    let (raw_key, raw_value) = entry
        .split_once('=')
        .ok_or_else(|| format!("Invalid metadata entry (expected key=value): {entry}"))?;

    let key = raw_key.trim();
    let value = raw_value.trim();

    if key.is_empty() {
        return Err(format!("Metadata key cannot be empty: {entry}"));
    }
    if value.is_empty() {
        return Err(format!("Metadata value cannot be empty for key: {key}"));
    }

    Ok((key.to_string(), infer_cli_value(value)))
}

pub fn infer_cli_value(text: &str) -> Value {
    let trimmed = text.trim();

    match trimmed {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }

    if let Ok(v) = trimmed.parse::<i64>() {
        return Value::Number(Number::from(v));
    }

    if let Ok(v) = trimmed.parse::<u64>() {
        return Value::Number(Number::from(v));
    }

    // Supports scientific notation if parsing is easy / available.
    if let Ok(v) = trimmed.parse::<f64>() {
        if v.is_finite() {
            if let Some(n) = Number::from_f64(v) {
                return Value::Number(n);
            }
        }
    }

    Value::String(trimmed.to_string())
}

pub fn validate_metadata(metadata: &UserMetadata) -> Result<(), String> {
    for (key, value) in metadata {
        validate_key(key)?;
        validate_value_recursive(value, key)?;
    }
    Ok(())
}

fn validate_value_recursive(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                validate_key(key)?;
                let nested_path = format!("{path}.{key}");
                validate_value_recursive(nested, &nested_path)?;
            }
            Ok(())
        }
        Value::Array(values) => {
            for (i, nested) in values.iter().enumerate() {
                let nested_path = format!("{path}[{i}]");
                validate_value_recursive(nested, &nested_path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_key(key: &str) -> Result<(), String> {
    for prefix in RESERVED_PREFIXES {
        if key.starts_with(prefix) {
            return Err(format!(
                "Metadata key '{key}' is invalid: keys may not start with '{prefix}'"
            ));
        }
    }
    Ok(())
}

pub fn canonical_json(metadata: &UserMetadata) -> Result<String, String> {
    serde_json::to_string(metadata)
        .map_err(|e| format!("Failed to serialize metadata to JSON: {e}"))
}

pub fn sanitize_key_for_netcdf(key: &str) -> Result<String, String> {
    let lowered = key.to_lowercase();

    let separator = '_';
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_separator = false;

    for ch in lowered.chars() {
        let is_alnum = ch.is_ascii_alphanumeric();
        if is_alnum {
            out.push(ch);
            last_was_separator = false;
        } else if !last_was_separator {
            out.push(separator);
            last_was_separator = true;
        }
    }

    let out = out.trim_matches(separator).to_string();

    if out.is_empty() {
        return Err(format!(
            "Metadata key '{key}' becomes empty after NetCDF sanitization"
        ));
    }

    Ok(out)
}

pub fn flatten_for_netcdf(
    metadata: &UserMetadata,
) -> Result<Vec<FlattenedMetadataAttribute>, String> {
    let mut used_names = BTreeSet::new();
    let mut out = Vec::with_capacity(metadata.len());

    for (key, value) in metadata {
        let sanitized = sanitize_key_for_netcdf(key)?;
        let attr_name = format!("meta_{sanitized}");

        if !used_names.insert(attr_name.clone()) {
            return Err(format!(
                "Two metadata keys collide after NetCDF sanitization at attribute '{attr_name}'"
            ));
        }

        out.push(FlattenedMetadataAttribute {
            name: attr_name,
            value: flatten_value_for_netcdf(value)?,
        });
    }

    Ok(out)
}

fn flatten_value_for_netcdf(value: &Value) -> Result<FlattenedMetadataValue, String> {
    match value {
        Value::Null => Ok(FlattenedMetadataValue::String("null".to_string())),
        Value::Bool(v) => Ok(FlattenedMetadataValue::U8(if *v { 1 } else { 0 })),
        Value::Number(n) => {
            if let Some(v) = n.as_i64() {
                Ok(FlattenedMetadataValue::I64(v))
            } else if let Some(v) = n.as_u64() {
                if v <= i64::MAX as u64 {
                    Ok(FlattenedMetadataValue::I64(v as i64))
                } else {
                    Ok(FlattenedMetadataValue::String(v.to_string()))
                }
            } else if let Some(v) = n.as_f64() {
                Ok(FlattenedMetadataValue::F64(v))
            } else {
                Err(format!("Unsupported JSON number representation: {n}"))
            }
        }
        Value::String(s) => Ok(FlattenedMetadataValue::String(s.clone())),
        // Per your latest instruction: still mirror nested objects/arrays, but stringify them
        // at the first level in the meta-* attributes.
        Value::Array(_) | Value::Object(_) => {
            let json = serde_json::to_string(value)
                .map_err(|e| format!("Failed to serialize nested metadata value: {e}"))?;
            Ok(FlattenedMetadataValue::String(json))
        }
    }
}

pub fn merge_prefer_first(dst: &mut UserMetadata, src: &UserMetadata) {
    for (key, value) in src {
        match dst.get_mut(key) {
            None => {
                dst.insert(key.clone(), value.clone());
            }
            Some(existing) => merge_value_prefer_first(existing, value),
        }
    }
}

fn merge_value_prefer_first(dst: &mut Value, src: &Value) {
    if let (Value::Object(dst_map), Value::Object(src_map)) = (dst, src) {
        merge_object_prefer_first(dst_map, src_map);
    }
}

fn merge_object_prefer_first(dst: &mut Map<String, Value>, src: &Map<String, Value>) {
    for (key, value) in src {
        match dst.get_mut(key) {
            None => {
                dst.insert(key.clone(), value.clone());
            }
            Some(existing) => merge_value_prefer_first(existing, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_cli_metadata() {
        let input = vec![
            "project = svalbard".to_string(),
            "year=2026".to_string(),
            "published=true".to_string(),
            "missing=null".to_string(),
            "floaty=1e-3".to_string(),
            "list=[1,2,3]".to_string(),
        ];
        let md = parse_cli_metadata(&input).unwrap();
        assert_eq!(md["project"], json!("svalbard"));
        assert_eq!(md["year"], json!(2026));
        assert_eq!(md["published"], json!(true));
        assert_eq!(md["missing"], Value::Null);
        assert_eq!(md["floaty"], json!(1e-3));
        assert_eq!(md["list"], json!("[1,2,3]"));
    }

    #[test]
    fn test_parse_cli_empty_value_fails() {
        let err = parse_cli_metadata(&["a=".to_string()]).unwrap_err();
        assert!(err.contains("cannot be empty"));
    }

    #[test]
    fn test_reserved_prefix_fails() {
        let err = parse_cli_metadata(&["meta_foo=bar".to_string()]).unwrap_err();
        assert!(err.contains("may not start with"));
    }

    #[test]
    fn test_sanitize_key() {
        assert_eq!(sanitize_key_for_netcdf("Some Key!").unwrap(), "some_key");
        assert_eq!(sanitize_key_for_netcdf("___A___B___").unwrap(), "a_b");
    }

    #[test]
    fn test_flatten_nested_to_json_string() {
        let md = UserMetadata::from([
            ("nested".to_string(), json!({"key2": 1})),
            ("arr".to_string(), json!([1, 2])),
            ("flag".to_string(), json!(true)),
            ("none".to_string(), Value::Null),
        ]);
        let flat = flatten_for_netcdf(&md).unwrap();
        let by_name = flat
            .into_iter()
            .map(|x| (x.name, x.value))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            by_name["meta_nested"],
            FlattenedMetadataValue::String("{\"key2\":1}".to_string())
        );
        assert_eq!(
            by_name["meta_arr"],
            FlattenedMetadataValue::String("[1,2]".to_string())
        );
        assert_eq!(by_name["meta_flag"], FlattenedMetadataValue::U8(1));
        assert_eq!(
            by_name["meta_none"],
            FlattenedMetadataValue::String("null".to_string())
        );
    }

    #[test]
    fn test_merge_prefer_first() {
        let mut a = value_to_metadata(json!({
            "x": 1,
            "nested": {
                "a": 1
            }
        }))
        .unwrap();

        let b = value_to_metadata(json!({
            "x": 999,
            "y": 2,
            "nested": {
                "a": 999,
                "b": 2
            }
        }))
        .unwrap();

        merge_prefer_first(&mut a, &b);

        assert_eq!(a["x"], json!(1));
        assert_eq!(a["y"], json!(2));
        assert_eq!(a["nested"]["a"], json!(1));
        assert_eq!(a["nested"]["b"], json!(2));
    }
}
