use super::CaptureError;
use crate::model::{AccountLimit, AgentStatus, SessionInfo, Tool};
use serde::Deserialize;

const MAX_REPORT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct Envelope {
    status: String,
    num_turns: Option<u64>,
    usage: Option<JsonObject<Usage>>,
    command: Option<JsonObject<Command>>,
    error: Option<ReportError>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    thinking_tokens: u64,
    cache_read_tokens: u64,
    total_tokens: u64,
}

impl Usage {
    fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.thinking_tokens == 0
            && self.cache_read_tokens == 0
            && self.total_tokens == 0
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReportError {
    Marker(String),
    Structured(JsonObject<ErrorCode>),
}

#[derive(Deserialize)]
struct ErrorCode {
    code: String,
}

impl ReportError {
    fn requires_login(&self) -> bool {
        match self {
            Self::Marker(code) | Self::Structured(JsonObject(ErrorCode { code })) => {
                code == "AUTH_REQUIRED"
            }
        }
    }
}

#[derive(Deserialize)]
struct Command {
    name: String,
    data: JsonObject<CommandData>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
// Serde 구조체의 위치 기반 배열 입력을 허용하지 않는다.
struct JsonObject<T>(#[serde(deserialize_with = "object")] T);

fn object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}

#[derive(Deserialize)]
struct CommandData {
    groups: Vec<JsonObject<Group>>,
}

#[derive(Deserialize)]
struct Group {
    name: String,
    #[serde(default)]
    buckets: Vec<JsonObject<Bucket>>,
}

#[derive(Deserialize)]
struct Bucket {
    id: String,
    // 알 수 없는 풀은 무시하고, 채택한 Gemini 풀만 값 타입을 검증한다.
    #[serde(default)]
    remaining_fraction: serde_json::Value,
    #[serde(default)]
    reset_time: serde_json::Value,
}

impl Bucket {
    fn into_limit(self, label: &str) -> Result<AccountLimit, CaptureError> {
        let remaining = self
            .remaining_fraction
            .as_f64()
            .ok_or(CaptureError::Unavailable)?;
        if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
            return Err(CaptureError::Unavailable);
        }
        let resets_at = match self.reset_time {
            serde_json::Value::Null => None,
            serde_json::Value::String(reset) => {
                chrono::DateTime::parse_from_rfc3339(&reset)
                    .map_err(|_| CaptureError::Unavailable)?;
                Some(reset)
            }
            _ => return Err(CaptureError::Unavailable),
        };
        Ok(AccountLimit {
            label: label.into(),
            used_percent: Some(((1.0 - remaining) * 100.0) as f32),
            resets_at,
        })
    }
}

/// 구조화된 메타데이터의 0 turn/0 token 증거와 Gemini 계정 한도를 검증한다.
/// 원문과 response는 보관하지 않으며, 프로세스·계정·현재 정책의 검증은 호출자 책임이다.
pub fn parse_report(
    body: &[u8],
    pc_id: &str,
    captured_at: &str,
) -> Result<AgentStatus, CaptureError> {
    if body.len() > MAX_REPORT_BYTES {
        return Err(CaptureError::Unavailable);
    }
    let JsonObject(envelope): JsonObject<Envelope> =
        serde_json::from_slice(body).map_err(|_| CaptureError::Unavailable)?;
    if envelope.status != "SUCCESS" {
        return Err(
            if envelope.status == "ERROR"
                && envelope
                    .error
                    .as_ref()
                    .is_some_and(ReportError::requires_login)
            {
                CaptureError::LoginRequired
            } else {
                CaptureError::Unavailable
            },
        );
    }
    if envelope.error.is_some()
        || envelope.num_turns != Some(0)
        || !envelope
            .usage
            .as_ref()
            .is_some_and(|usage| usage.0.is_zero())
    {
        return Err(CaptureError::Unavailable);
    }
    let JsonObject(command) = envelope.command.ok_or(CaptureError::Unavailable)?;
    if command.name != "usage" {
        return Err(CaptureError::Unavailable);
    }
    let mut gemini_seen = false;
    let mut primary = None;
    let mut secondary = None;
    for JsonObject(group) in command.data.0.groups {
        if group.name != "Gemini Models" {
            continue;
        }
        if gemini_seen {
            return Err(CaptureError::Unavailable);
        }
        gemini_seen = true;
        for JsonObject(bucket) in group.buckets {
            let (slot, label) = match bucket.id.as_str() {
                "gemini-5h" => (&mut primary, "5h"),
                "gemini-weekly" => (&mut secondary, "week"),
                _ => continue,
            };
            if slot.is_some() {
                return Err(CaptureError::Unavailable);
            }
            *slot = Some(bucket.into_limit(label)?);
        }
    }
    if primary.is_none() && secondary.is_none() {
        return Err(CaptureError::Unavailable);
    }
    Ok(AgentStatus {
        schema_version: "agent_status.v1".into(),
        pc_id: pc_id.into(),
        tool: Tool::Antigravity,
        session_id: "antigravity-cli-account".into(),
        captured_at: captured_at.into(),
        primary,
        secondary,
        session: SessionInfo {
            active: true,
            context_used_percent: None,
        },
        cost_estimate_usd: None,
        approx: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const PC_ID: &str = "synthetic-pc";
    const CAPTURED_AT: &str = "2026-01-02T03:04:05Z";
    const COUNTERS: [&str; 5] = [
        "input_tokens",
        "output_tokens",
        "thinking_tokens",
        "cache_read_tokens",
        "total_tokens",
    ];

    fn report() -> Value {
        json!({
            "conversation_id": "synthetic-conversation",
            "status": "SUCCESS",
            "response": "synthetic text, not quota evidence",
            "duration_seconds": 0.125,
            "num_turns": 0,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "thinking_tokens": 0,
                "cache_read_tokens": 0,
                "total_tokens": 0
            },
            "command": {
                "name": "usage",
                "data": {
                    "unknown_plan": "synthetic-plan",
                    "groups": [{
                        "name": "Gemini Models",
                        "description": "synthetic-description",
                        "buckets": [{
                            "id": "gemini-weekly",
                            "name": "synthetic-week",
                            "window": "synthetic-window",
                            "remaining_fraction": 0.125,
                            "reset_time": "2026-01-09T12:34:56+09:00"
                        }, {
                            "id": "gemini-5h",
                            "remaining_fraction": 0.75,
                            "reset_time": "2026-01-02T08:04:05Z"
                        }]
                    }]
                }
            }
        })
    }

    fn parse(report: &Value) -> Result<AgentStatus, CaptureError> {
        parse_report(&serde_json::to_vec(report).unwrap(), PC_ID, CAPTURED_AT)
    }

    fn buckets(report: &mut Value) -> &mut Vec<Value> {
        report["command"]["data"]["groups"][0]["buckets"]
            .as_array_mut()
            .unwrap()
    }

    fn unavailable(report: &Value) {
        assert_eq!(parse(report).err(), Some(CaptureError::Unavailable));
    }

    #[test]
    fn valid_report_maps_fixed_periods_and_sanitized_metadata() {
        let status = parse(&report()).unwrap();
        assert_eq!(status.schema_version, "agent_status.v1");
        assert_eq!(status.pc_id, PC_ID);
        assert_eq!(status.captured_at, CAPTURED_AT);
        assert_eq!(status.tool, Tool::Antigravity);
        assert_eq!(status.session_id, "antigravity-cli-account");
        let primary = status.primary.as_ref().unwrap();
        assert_eq!(primary.label, "5h");
        assert_eq!(primary.used_percent, Some(25.0));
        assert_eq!(primary.resets_at.as_deref(), Some("2026-01-02T08:04:05Z"));
        let secondary = status.secondary.as_ref().unwrap();
        assert_eq!(secondary.label, "week");
        assert_eq!(secondary.used_percent, Some(87.5));
        assert_eq!(
            secondary.resets_at.as_deref(),
            Some("2026-01-09T12:34:56+09:00")
        );
        assert!(status.session.active);
        assert!(status.session.context_used_percent.is_none());
        assert!(status.cost_estimate_usd.is_none());
        assert!(!status.approx);
        let serialized = serde_json::to_string(&status).unwrap();
        for ignored in [
            "synthetic-conversation",
            "synthetic text",
            "synthetic-plan",
            "synthetic-description",
            "synthetic-window",
        ] {
            assert!(!serialized.contains(ignored));
        }
    }

    #[test]
    fn missing_period_is_not_invented_and_reset_is_optional() {
        for id in ["gemini-5h", "gemini-weekly"] {
            for reset in [None, Some(Value::Null)] {
                let mut body = report();
                *buckets(&mut body) = vec![json!({"id": id, "remaining_fraction": 0.125})];
                if let Some(reset) = reset {
                    buckets(&mut body)[0]["reset_time"] = reset;
                }
                let status = parse(&body).unwrap();
                let limit = if id == "gemini-5h" {
                    assert!(status.secondary.is_none());
                    status.primary.unwrap()
                } else {
                    assert!(status.primary.is_none());
                    status.secondary.unwrap()
                };
                assert_eq!(limit.used_percent, Some(87.5));
                assert!(limit.resets_at.is_none());
            }
        }
    }

    #[test]
    fn fraction_endpoints_and_bucket_order_are_preserved() {
        for (remaining, used) in [(0, 100.0), (1, 0.0)] {
            let mut body = report();
            for bucket in buckets(&mut body).iter_mut() {
                bucket["remaining_fraction"] = json!(remaining);
            }
            buckets(&mut body).reverse();
            let status = parse(&body).unwrap();
            assert_eq!(status.primary.unwrap().used_percent, Some(used));
            assert_eq!(status.secondary.unwrap().used_percent, Some(used));
        }
    }

    #[test]
    fn response_and_unrelated_fields_are_never_quota_or_auth_evidence() {
        for response in [
            json!("AUTH_REQUIRED: Gemini Models gemini-5h 100%"),
            json!({"status": "ERROR", "error": "AUTH_REQUIRED"}),
            Value::Null,
        ] {
            let mut body = report();
            body["response"] = response;
            body["context_window"] = json!({"remaining_percentage": 12});
            assert_eq!(
                parse(&body).unwrap().primary.unwrap().used_percent,
                Some(25.0)
            );
            body.as_object_mut().unwrap().remove("command");
            unavailable(&body);
        }
        let mut body = report();
        body.as_object_mut().unwrap().remove("response");
        assert!(parse(&body).is_ok());
    }

    #[test]
    fn unknown_ids_and_third_party_groups_are_ignored() {
        let mut body = report();
        buckets(&mut body).push(json!({
            "id": "gemini-future", "remaining_fraction": "ignored", "reset_time": false
        }));
        buckets(&mut body).push(json!({"id": "3p-weekly", "remaining_fraction": null}));
        buckets(&mut body).push(json!({"id": "", "remaining_fraction": "ignored"}));
        for name in ["Third-party Models", "Claude Models", "GPT Models"] {
            body["command"]["data"]["groups"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "name": name,
                    "buckets": [
                        {"id": "gemini-5h", "remaining_fraction": 0.0},
                        {"id": "gemini-weekly", "remaining_fraction": "ignored"},
                        {"id": "3p-weekly", "remaining_fraction": 0.01}
                    ]
                }));
        }
        let status = parse(&body).unwrap();
        assert_eq!(status.primary.unwrap().used_percent, Some(25.0));
        assert_eq!(status.secondary.unwrap().used_percent, Some(87.5));
    }

    #[test]
    fn exact_gemini_group_and_at_least_one_known_period_are_required() {
        for name in [
            "Third-party Models",
            "Gemini",
            "gemini models",
            "Gemini Models ",
        ] {
            let mut body = report();
            body["command"]["data"]["groups"][0]["name"] = json!(name);
            unavailable(&body);
        }
        for entries in [
            json!([]),
            json!([{"id": "3p-weekly", "remaining_fraction": 0.5}]),
            json!([{"id": "gemini-week", "remaining_fraction": 0.5}]),
        ] {
            let mut body = report();
            body["command"]["data"]["groups"][0]["buckets"] = entries;
            unavailable(&body);
        }
        let mut body = report();
        body["command"]["data"]["groups"] = json!([]);
        unavailable(&body);
    }

    #[test]
    fn duplicate_known_periods_and_gemini_groups_fail_closed() {
        for index in [0, 1] {
            let mut body = report();
            let duplicate = buckets(&mut body)[index].clone();
            buckets(&mut body).push(duplicate);
            unavailable(&body);
        }
        for empty in [false, true] {
            let mut body = report();
            let mut duplicate = body["command"]["data"]["groups"][0].clone();
            if empty {
                duplicate["buckets"] = json!([]);
            }
            body["command"]["data"]["groups"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
            unavailable(&body);
        }
    }

    #[test]
    fn fractions_must_be_present_finite_numbers_in_range() {
        for value in [
            json!(-0.01),
            json!(1.01),
            json!("NaN"),
            json!("0.5"),
            Value::Null,
            json!(true),
            json!([]),
            json!({}),
        ] {
            for index in [0, 1] {
                let mut body = report();
                buckets(&mut body)[index]["remaining_fraction"] = value.clone();
                unavailable(&body);
            }
        }
        for index in [0, 1] {
            let mut body = report();
            buckets(&mut body)[index]
                .as_object_mut()
                .unwrap()
                .remove("remaining_fraction");
            unavailable(&body);
        }
        for value in ["NaN", "Infinity", "-Infinity", "1e999"] {
            let raw = serde_json::to_string(&report()).unwrap().replacen(
                "\"remaining_fraction\":0.125",
                &format!("\"remaining_fraction\":{value}"),
                1,
            );
            assert_eq!(
                parse_report(raw.as_bytes(), PC_ID, CAPTURED_AT).err(),
                Some(CaptureError::Unavailable)
            );
        }
    }

    #[test]
    fn reset_must_be_rfc3339_when_present() {
        for reset in [
            json!("invalid"),
            json!(""),
            json!("2026-01-02"),
            json!("2026-01-02T08:04:05"),
            json!(0),
            json!(false),
            json!({}),
            json!([]),
        ] {
            for index in [0, 1] {
                let mut body = report();
                buckets(&mut body)[index]["reset_time"] = reset.clone();
                unavailable(&body);
            }
        }
    }

    #[test]
    fn model_turns_and_any_token_usage_are_rejected() {
        let mut body = report();
        body["num_turns"] = json!(1);
        unavailable(&body);
        for counter in COUNTERS {
            let mut body = report();
            body["usage"][counter] = json!(1);
            unavailable(&body);
        }
    }

    #[test]
    fn all_zero_usage_proof_fields_are_mandatory_and_integer_typed() {
        for field in ["num_turns", "usage", "command", "status"] {
            let mut body = report();
            body.as_object_mut().unwrap().remove(field);
            unavailable(&body);
            let mut body = report();
            body[field] = Value::Null;
            unavailable(&body);
        }
        for counter in COUNTERS {
            let mut body = report();
            body["usage"].as_object_mut().unwrap().remove(counter);
            unavailable(&body);
            for value in [
                Value::Null,
                json!("0"),
                json!(0.0),
                json!(-1),
                json!(false),
                json!([]),
            ] {
                let mut body = report();
                body["usage"][counter] = value;
                unavailable(&body);
            }
        }
        for value in [json!("0"), json!(0.0), json!(-1), json!(false), json!({})] {
            let mut body = report();
            body["num_turns"] = value;
            unavailable(&body);
        }
    }

    #[test]
    fn overflowing_turn_and_token_counters_fail_closed() {
        for field in std::iter::once("num_turns").chain(COUNTERS) {
            let raw = serde_json::to_string(&report()).unwrap().replacen(
                &format!("\"{field}\":0"),
                &format!("\"{field}\":18446744073709551616"),
                1,
            );
            assert_eq!(
                parse_report(raw.as_bytes(), PC_ID, CAPTURED_AT).err(),
                Some(CaptureError::Unavailable)
            );
        }
    }

    #[test]
    fn only_success_and_the_exact_usage_command_are_accepted() {
        for status in ["ERROR", "success", "PENDING", "AUTH_REQUIRED", ""] {
            let mut body = report();
            body["status"] = json!(status);
            unavailable(&body);
        }
        for name in ["quota", "/usage", "Usage", "usage ", "chat", ""] {
            let mut body = report();
            body["command"]["name"] = json!(name);
            unavailable(&body);
        }
        let mut body = report();
        body["error"] = json!("AUTH_REQUIRED");
        unavailable(&body);
    }

    #[test]
    fn required_command_and_quota_shapes_are_well_typed() {
        for pointer in [
            "/command/name",
            "/command/data",
            "/command/data/groups",
            "/command/data/groups/0/name",
            "/command/data/groups/0/buckets",
            "/command/data/groups/0/buckets/0/id",
        ] {
            for value in [Value::Null, json!(1), json!(true), json!([]), json!({})] {
                let mut body = report();
                *body.pointer_mut(pointer).unwrap() = value;
                assert_eq!(
                    parse(&body).err(),
                    Some(CaptureError::Unavailable),
                    "invalid shape at {pointer}"
                );
            }
            let mut body = report();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            body.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            unavailable(&body);
        }
    }

    #[test]
    fn positional_arrays_cannot_replace_native_objects() {
        for (pointer, value) in [
            ("/usage", json!([0, 0, 0, 0, 0])),
            (
                "/command",
                json!(["usage", report()["command"]["data"].clone()]),
            ),
            (
                "/command/data",
                json!([report()["command"]["data"]["groups"].clone()]),
            ),
        ] {
            let mut body = report();
            *body.pointer_mut(pointer).unwrap() = value;
            assert_eq!(
                parse(&body).err(),
                Some(CaptureError::Unavailable),
                "array at {pointer}"
            );
        }
        unavailable(&json!([
            "SUCCESS",
            0,
            report()["usage"].clone(),
            report()["command"].clone(),
            null
        ]));
        unavailable(&json!({"status": "ERROR", "error": ["AUTH_REQUIRED"]}));
    }

    #[test]
    fn status_and_pool_identifiers_cannot_be_numeric_variant_indices() {
        for pointer in [
            "/status",
            "/command/data/groups/0/name",
            "/command/data/groups/0/buckets/0/id",
        ] {
            for value in [0, 1] {
                let mut body = report();
                *body.pointer_mut(pointer).unwrap() = json!(value);
                unavailable(&body);
            }
        }
    }

    #[test]
    fn authentication_requires_an_explicit_machine_error_marker() {
        for error in [json!("AUTH_REQUIRED"), json!({"code": "AUTH_REQUIRED"})] {
            let body = json!({"status": "ERROR", "error": error});
            assert_eq!(parse(&body).err(), Some(CaptureError::LoginRequired));
        }
        for error in [
            Value::Null,
            json!("authentication required"),
            json!("AUTH_REQUIRED details"),
            json!({"message": "AUTH_REQUIRED"}),
            json!({"code": "UNKNOWN", "message": "AUTH_REQUIRED"}),
            json!({"code": 401}),
            json!(true),
        ] {
            let body = json!({"status": "ERROR", "error": error, "response": "AUTH_REQUIRED"});
            unavailable(&body);
        }
        unavailable(&json!({"status": "ERROR", "response": "AUTH_REQUIRED"}));
        unavailable(&json!({"status": "PENDING", "error": "AUTH_REQUIRED"}));
        for raw in [
            br#"{"status":"ERROR","error":{"code":"UNKNOWN","code":"AUTH_REQUIRED"}}"#.as_slice(),
            br#"{"status":"ERROR","error":"UNKNOWN","error":"AUTH_REQUIRED"}"#,
        ] {
            assert_eq!(
                parse_report(raw, PC_ID, CAPTURED_AT).err(),
                Some(CaptureError::Unavailable)
            );
        }
    }

    #[test]
    fn duplicate_proof_and_known_quota_fields_are_rejected() {
        for (key, value) in [
            ("status", json!("SUCCESS")),
            ("num_turns", json!(0)),
            ("usage", report()["usage"].clone()),
            ("command", report()["command"].clone()),
            ("input_tokens", json!(0)),
            ("name", json!("usage")),
            ("data", report()["command"]["data"].clone()),
            ("groups", report()["command"]["data"]["groups"].clone()),
            (
                "buckets",
                report()["command"]["data"]["groups"][0]["buckets"].clone(),
            ),
            ("id", json!("gemini-weekly")),
            ("remaining_fraction", json!(0.125)),
            ("reset_time", json!("2026-01-09T12:34:56+09:00")),
        ] {
            let field = format!("\"{key}\":{}", serde_json::to_string(&value).unwrap());
            let raw = serde_json::to_string(&report()).unwrap().replacen(
                &field,
                &format!("{field},{field}"),
                1,
            );
            assert_eq!(
                parse_report(raw.as_bytes(), PC_ID, CAPTURED_AT).err(),
                Some(CaptureError::Unavailable),
                "duplicate {key}"
            );
        }
        for name in ["Gemini Models", "Third-party Models"] {
            let raw = serde_json::to_string(&report()).unwrap().replacen(
                "\"name\":\"Gemini Models\"",
                &format!("\"name\":\"{name}\",\"name\":\"Gemini Models\""),
                1,
            );
            assert_eq!(
                parse_report(raw.as_bytes(), PC_ID, CAPTURED_AT).err(),
                Some(CaptureError::Unavailable)
            );
        }
    }

    #[test]
    fn malformed_json_and_size_overflow_fail_closed() {
        for bytes in [
            b"".as_slice(),
            b"not json",
            b"[]",
            b"null",
            b"{}",
            b"{",
            b"\xff",
        ] {
            assert_eq!(
                parse_report(bytes, PC_ID, CAPTURED_AT).err(),
                Some(CaptureError::Unavailable)
            );
        }
        let mut body = serde_json::to_vec(&report()).unwrap();
        body.resize(MAX_REPORT_BYTES, b' ');
        assert!(parse_report(&body, PC_ID, CAPTURED_AT).is_ok());
        body.push(b' ');
        assert_eq!(
            parse_report(&body, PC_ID, CAPTURED_AT).err(),
            Some(CaptureError::Unavailable)
        );
        let mut body = serde_json::to_vec(&report()).unwrap();
        body.extend_from_slice(b"{}");
        assert_eq!(
            parse_report(&body, PC_ID, CAPTURED_AT).err(),
            Some(CaptureError::Unavailable)
        );
    }
}
