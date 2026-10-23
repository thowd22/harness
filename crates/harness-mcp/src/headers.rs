//! Request-metadata headers for streamable HTTP (MCP 2026-07-28):
//! `Mcp-Name` and `Mcp-Param-{Name}` mirrored from tool arguments marked
//! with `x-mcp-header`, with the `=?base64?…?=` value encoding.

use base64::Engine;
use serde_json::Value;

const SENTINEL_PREFIX: &str = "=?base64?";
const SENTINEL_SUFFIX: &str = "?=";

/// Encodes `value` for an `Mcp-Name` / `Mcp-Param-*` header: as-is when it
/// is plain header-safe ASCII, otherwise Base64 in the sentinel format.
pub fn encode_value(value: &str) -> String {
    let safe = value
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
        && value.trim() == value
        && !(value.starts_with(SENTINEL_PREFIX) && value.ends_with(SENTINEL_SUFFIX));
    if safe {
        value.to_owned()
    } else {
        format!(
            "{SENTINEL_PREFIX}{}{SENTINEL_SUFFIX}",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    }
}

/// Decodes a header value written by [`encode_value`].
pub fn decode_value(value: &str) -> Option<String> {
    match value
        .strip_prefix(SENTINEL_PREFIX)
        .and_then(|v| v.strip_suffix(SENTINEL_SUFFIX))
    {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(value.to_owned()),
    }
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// One `x-mcp-header` annotation: the header name part and the property
/// path from the schema root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamHeader {
    /// The `{name}` in `Mcp-Param-{name}`.
    pub name: String,
    /// Property names from the root of the arguments object.
    pub path: Vec<String>,
}

/// Counts every `x-mcp-header` key anywhere in `schema`.
fn count_annotations(schema: &Value) -> usize {
    match schema {
        Value::Object(m) => {
            usize::from(m.contains_key("x-mcp-header"))
                + m.values().map(count_annotations).sum::<usize>()
        }
        Value::Array(a) => a.iter().map(count_annotations).sum(),
        _ => 0,
    }
}

/// Collects annotations reachable through `properties` chains only.
fn collect(schema: &Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, Value)>) {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    for (key, prop) in props {
        path.push(key.clone());
        if prop.get("x-mcp-header").is_some() {
            out.push((path.clone(), prop.clone()));
        }
        collect(prop, path, out);
        path.pop();
    }
}

/// Validates the `x-mcp-header` annotations of a tool's `inputSchema` and
/// returns them, or why the tool definition is invalid.
pub fn param_headers(input_schema: &Value) -> Result<Vec<ParamHeader>, String> {
    let mut found = Vec::new();
    collect(input_schema, &mut Vec::new(), &mut found);
    if found.len() != count_annotations(input_schema) {
        return Err(
            "`x-mcp-header` is only allowed on properties reachable through `properties`".into(),
        );
    }
    let mut out: Vec<ParamHeader> = Vec::new();
    for (path, prop) in found {
        let name = prop
            .get("x-mcp-header")
            .and_then(Value::as_str)
            .ok_or("`x-mcp-header` must be a string")?;
        if !is_token(name) {
            return Err(format!(
                "`x-mcp-header` value `{name}` is not a valid header name"
            ));
        }
        if out.iter().any(|h| h.name.eq_ignore_ascii_case(name)) {
            return Err(format!("`x-mcp-header` value `{name}` is used twice"));
        }
        match prop.get("type").and_then(Value::as_str) {
            Some("string" | "integer" | "boolean") => {}
            _ => {
                return Err(format!(
                    "`x-mcp-header` `{name}` must be on a string, integer or boolean property"
                ));
            }
        }
        out.push(ParamHeader {
            name: name.to_owned(),
            path,
        });
    }
    Ok(out)
}

/// The `Mcp-Param-*` headers for a call with `arguments`.
pub fn param_header_values(headers: &[ParamHeader], arguments: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for h in headers {
        let mut v = arguments;
        let mut present = true;
        for key in &h.path {
            match v.get(key) {
                Some(next) => v = next,
                None => {
                    present = false;
                    break;
                }
            }
        }
        if !present {
            continue;
        }
        let text = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
            _ => continue,
        };
        out.push((format!("Mcp-Param-{}", h.name), encode_value(&text)));
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn encodes_like_the_spec_examples() {
        assert_eq!(encode_value("us-west1"), "us-west1");
        assert_eq!(
            encode_value("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        assert_eq!(encode_value(" padded "), "=?base64?IHBhZGRlZCA=?=");
        assert_eq!(encode_value("line1\nline2"), "=?base64?bGluZTEKbGluZTI=?=");
        assert_eq!(
            encode_value("=?base64?literal?="),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
        assert_eq!(
            decode_value("=?base64?SGVsbG8sIOS4lueVjA==?=").unwrap(),
            "Hello, 世界"
        );
        assert_eq!(decode_value("plain").unwrap(), "plain");
    }

    #[test]
    fn annotations_are_validated() {
        let ok = json!({"type":"object","properties":{
            "region":{"type":"string","x-mcp-header":"Region"},
            "opts":{"type":"object","properties":{"n":{"type":"integer","x-mcp-header":"N"}}}
        }});
        let h = param_headers(&ok).unwrap();
        assert_eq!(h.len(), 2);
        let vals = param_header_values(&h, &json!({"region":"eu","opts":{"n":3}}));
        assert!(vals.contains(&("Mcp-Param-Region".into(), "eu".into())));
        assert!(vals.contains(&("Mcp-Param-N".into(), "3".into())));
        assert!(param_header_values(&h, &json!({"region":null})).is_empty());

        for bad in [
            json!({"properties":{"x":{"type":"number","x-mcp-header":"X"}}}),
            json!({"properties":{"x":{"type":"string","x-mcp-header":"bad name"}}}),
            json!({"properties":{"x":{"type":"string","x-mcp-header":"A"},"y":{"type":"string","x-mcp-header":"a"}}}),
            json!({"properties":{"x":{"type":"array","items":{"type":"string","x-mcp-header":"X"}}}}),
            json!({"anyOf":[{"properties":{"x":{"type":"string","x-mcp-header":"X"}}}]}),
        ] {
            assert!(param_headers(&bad).is_err(), "{bad}");
        }
    }
}
