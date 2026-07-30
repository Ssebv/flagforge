//! The subject a flag is evaluated for.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::value::AttributeValue;

/// Who (or what) we are deciding for.
///
/// `key` is the stable identity used for percentage bucketing — a user id, a
/// device id, an account id. Everything else lives in `attributes` and is only
/// read by targeting rules.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EvaluationContext {
    pub key: String,
    /// Ordered so that logs and snapshots of a context are byte-stable.
    #[serde(default)]
    pub attributes: BTreeMap<String, AttributeValue>,
}

impl EvaluationContext {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), attributes: BTreeMap::new() }
    }

    pub fn with(mut self, name: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
        self.attributes.insert(name.into(), value.into());
        self
    }

    pub fn attribute(&self, name: &str) -> Option<&AttributeValue> {
        self.attributes.get(name)
    }

    /// The string a rollout hashes on: either the named attribute or, by
    /// default, the context key itself.
    pub fn bucketing_subject(&self, bucket_by: Option<&str>) -> Option<String> {
        match bucket_by {
            None => Some(self.key.clone()),
            Some(name) => self.attributes.get(name).and_then(AttributeValue::to_text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucketing_falls_back_to_the_context_key() {
        let ctx = EvaluationContext::new("user-1").with("account_id", "acct-9");
        assert_eq!(ctx.bucketing_subject(None).unwrap(), "user-1");
        assert_eq!(ctx.bucketing_subject(Some("account_id")).unwrap(), "acct-9");
        assert_eq!(ctx.bucketing_subject(Some("missing")), None);
    }
}
