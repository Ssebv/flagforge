//! Dynamically typed values used by variants and by evaluation contexts.

use serde::{Deserialize, Serialize};

/// The payload a flag serves once a variant has been selected.
///
/// Deserialization is `untagged`, so the wire format is just plain JSON:
/// `true`, `42`, `"blue"` or `{"ratio": 0.3}` all round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum VariantValue {
    Bool(bool),
    Number(f64),
    String(String),
    /// Objects, arrays and `null`.
    Json(serde_json::Value),
}

impl VariantValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Json(serde_json::Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Json(serde_json::Value::Number(n)) => n.as_f64(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            Self::Json(serde_json::Value::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn null() -> Self {
        Self::Json(serde_json::Value::Null)
    }
}

impl From<bool> for VariantValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<&str> for VariantValue {
    fn from(v: &str) -> Self {
        Self::String(v.to_owned())
    }
}

impl From<f64> for VariantValue {
    fn from(v: f64) -> Self {
        Self::Number(v)
    }
}

/// A value carried by an [`EvaluationContext`](crate::EvaluationContext) or
/// compared against inside a targeting [`Condition`](crate::Condition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum AttributeValue {
    Bool(bool),
    Number(f64),
    String(String),
    /// A multi-valued attribute, e.g. a user's roles.
    ///
    /// This variant makes the type recursive; the schema generator is told to
    /// stop here rather than inline `AttributeValue` into itself forever.
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    List(Vec<AttributeValue>),
}

impl AttributeValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// String projection used by the textual operators. Numbers and booleans
    /// are rendered so that `version = 3` still matches `starts_with "3"`.
    pub fn to_text(&self) -> Option<String> {
        match self {
            Self::String(s) => Some(s.clone()),
            Self::Bool(b) => Some(b.to_string()),
            Self::Number(n) => Some(format_number(*n)),
            Self::List(_) => None,
        }
    }

    /// Flattens a scalar into a one-element slice and a list into its items,
    /// so `In`/`Equals` behave sensibly for multi-valued attributes.
    pub fn scalars(&self) -> Vec<&AttributeValue> {
        match self {
            Self::List(items) => items.iter().collect(),
            other => vec![other],
        }
    }
}

/// Renders a float without a trailing `.0` so integral values stringify the
/// way a user writing targeting rules would expect.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

impl From<bool> for AttributeValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<&str> for AttributeValue {
    fn from(v: &str) -> Self {
        Self::String(v.to_owned())
    }
}

impl From<String> for AttributeValue {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}

impl From<f64> for AttributeValue {
    fn from(v: f64) -> Self {
        Self::Number(v)
    }
}

impl From<i64> for AttributeValue {
    fn from(v: i64) -> Self {
        Self::Number(v as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_value_round_trips_through_json() {
        let cases = [
            ("true", VariantValue::Bool(true)),
            ("42.0", VariantValue::Number(42.0)),
            ("\"blue\"", VariantValue::String("blue".into())),
        ];
        for (raw, expected) in cases {
            let parsed: VariantValue = serde_json::from_str(raw).unwrap();
            assert_eq!(parsed, expected);
        }

        let object: VariantValue = serde_json::from_str(r#"{"ratio":0.3}"#).unwrap();
        assert!(matches!(object, VariantValue::Json(serde_json::Value::Object(_))));
    }

    #[test]
    fn integral_numbers_stringify_without_decimal_part() {
        assert_eq!(AttributeValue::Number(3.0).to_text().unwrap(), "3");
        assert_eq!(AttributeValue::Number(3.5).to_text().unwrap(), "3.5");
    }
}
