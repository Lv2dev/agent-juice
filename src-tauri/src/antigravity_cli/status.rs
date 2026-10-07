use crate::model::{AccountLimit, AgentStatus, SessionInfo, Tool};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use zeroize::Zeroize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureError {
    AppRequired,
    LoginRequired,
    Unavailable,
}

#[derive(Deserialize)]
struct Payload {
    product: String,
    email: Option<String>,
    #[serde(default, deserialize_with = "quota_map")]
    quota: Option<BTreeMap<String, Quota>>,
}

impl Drop for Payload {
    fn drop(&mut self) {
        if let Some(email) = &mut self.email {
            email.zeroize();
        }
    }
}

#[derive(Deserialize)]
struct Quota {
    remaining_fraction: f64,
    reset_time: Option<String>,
}

fn quota_map<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, Quota>>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Option<BTreeMap<String, Quota>>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a quota object")
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: serde::Deserializer<'de>>(
            self,
            value: D,
        ) -> Result<Self::Value, D::Error> {
            value.deserialize_map(self)
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut quotas = BTreeMap::new();
            let mut count = 0;
            while let Some(id) = map.next_key::<String>()? {
                count += 1;
                if count > 256 {
                    return Err(serde::de::Error::custom("too many quota entries"));
                }
                if id == "gemini-5h" || id == "gemini-weekly" {
                    let value = map.next_value::<Quota>()?;
                    if quotas.insert(id, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate quota"));
                    }
                } else {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
            Ok(Some(quotas))
        }
    }
    deserializer.deserialize_option(Visitor)
}

pub fn parse_payload(
    input: &[u8],
    binding_id: &str,
    pc_id: &str,
    captured_at: &str,
) -> Result<(AgentStatus, String), CaptureError> {
    if input.len() > crate::statusline::MAX_STATUSLINE_INPUT_BYTES {
        return Err(CaptureError::Unavailable);
    }
    let payload: Payload = serde_json::from_slice(input).map_err(|_| CaptureError::Unavailable)?;
    if payload.product != "antigravity" {
        return Err(CaptureError::Unavailable);
    }
    let email = payload
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty() && email.len() <= 512)
        .ok_or(CaptureError::LoginRequired)?;
    let normalized = zeroize::Zeroizing::new(email.to_lowercase());
    let mut digest = Sha256::new();
    digest.update(binding_id.as_bytes());
    digest.update([0]);
    digest.update(normalized.as_bytes());
    let account_hash = format!("{:x}", digest.finalize());
    let quotas = payload.quota.as_ref().ok_or(CaptureError::Unavailable)?;
    if quotas.len() > 256 {
        return Err(CaptureError::Unavailable);
    }
    let limit = |id: &str, label: &str| -> Result<Option<AccountLimit>, CaptureError> {
        let Some(quota) = quotas.get(id) else {
            return Ok(None);
        };
        if !quota.remaining_fraction.is_finite() || !(0.0..=1.0).contains(&quota.remaining_fraction)
        {
            return Err(CaptureError::Unavailable);
        }
        if let Some(reset) = &quota.reset_time {
            chrono::DateTime::parse_from_rfc3339(reset).map_err(|_| CaptureError::Unavailable)?;
        }
        Ok(Some(AccountLimit {
            label: label.into(),
            used_percent: Some(((1.0 - quota.remaining_fraction) * 100.0) as f32),
            resets_at: quota.reset_time.clone(),
        }))
    };
    let primary = limit("gemini-5h", "5h")?;
    let secondary = limit("gemini-weekly", "week")?;
    if primary.is_none() && secondary.is_none() {
        return Err(CaptureError::Unavailable);
    }
    Ok((
        AgentStatus {
            schema_version: "agent_status.v1".into(),
            pc_id: pc_id.into(),
            tool: Tool::Antigravity,
            session_id: "antigravity-cli".into(),
            captured_at: captured_at.into(),
            primary,
            secondary,
            session: SessionInfo {
                active: true,
                context_used_percent: None,
            },
            cost_estimate_usd: None,
            approx: false,
        },
        account_hash,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn payload(quota: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "product":"antigravity", "email":"person@example.invalid", "quota":quota,
            "context_window":{"remaining_percentage":12},
            "token":"not-for-storage", "messages":["not-for-storage"]
        }))
        .unwrap()
    }
    fn parse(quota: serde_json::Value) -> Result<(AgentStatus, String), CaptureError> {
        parse_payload(&payload(quota), "binding-a", "pc", "2026-10-06T03:12:00Z")
    }
    #[test]
    fn cli_periods_preserve_zero_and_ignore_context_and_other_pools() {
        let (status, hash) = parse(serde_json::json!({
            "gemini-weekly":{"remaining_fraction":0.75,"reset_time":"2026-10-13T00:00:00Z"},
            "gemini-5h":{"remaining_fraction":0.0},
            "3p-weekly":{"remaining_fraction":0.1}
        }))
        .unwrap();
        assert_eq!(status.primary.unwrap().used_percent, Some(100.0));
        assert_eq!(status.secondary.unwrap().used_percent, Some(25.0));
        assert!(status.session.context_used_percent.is_none());
        assert_eq!(hash.len(), 64);
    }
    #[test]
    fn cli_missing_periods_are_not_created_and_full_remaining_is_valid() {
        let (status, _) =
            parse(serde_json::json!({"gemini-weekly":{"remaining_fraction":1}})).unwrap();
        assert!(status.primary.is_none());
        assert_eq!(status.secondary.unwrap().used_percent, Some(0.0));
    }
    #[test]
    fn cli_invalid_missing_null_and_reset_values_fail_closed() {
        for quota in [
            serde_json::json!({}),
            serde_json::json!({"gemini-5h":{}}),
            serde_json::json!({"gemini-5h":{"remaining_fraction":null}}),
            serde_json::json!({"gemini-5h":{"remaining_fraction":1.1}}),
            serde_json::json!({"gemini-5h":{"remaining_fraction":-0.1}}),
            serde_json::json!({"gemini-5h":{"remaining_fraction":0.5,"reset_time":"invalid"}}),
        ] {
            assert_eq!(parse(quota).err(), Some(CaptureError::Unavailable));
        }
    }
    #[test]
    fn cli_auth_absence_is_not_parse_failure() {
        let input = br#"{"product":"antigravity","email":"","quota":{}}"#;
        assert_eq!(
            parse_payload(input, "binding", "pc", "now").err(),
            Some(CaptureError::LoginRequired)
        );
        assert_eq!(
            parse_payload(b"not json", "binding", "pc", "now").err(),
            Some(CaptureError::Unavailable)
        );
    }
    #[test]
    fn cli_account_fingerprint_is_binding_scoped_and_status_is_sanitized() {
        let bytes = payload(serde_json::json!({"gemini-5h":{"remaining_fraction":0.2}}));
        let (status, a) = parse_payload(&bytes, "a", "pc", "now").unwrap();
        let (_, b) = parse_payload(&bytes, "b", "pc", "now").unwrap();
        assert_ne!(a, b);
        let serialized = serde_json::to_string(&status).unwrap();
        for value in [
            "person@example.invalid",
            "not-for-storage",
            "messages",
            "token",
        ] {
            assert!(!serialized.contains(value));
        }
    }
}
