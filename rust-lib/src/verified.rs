//! eth_rpc's verified-proxy verdict as this backend relays it. eth_rpc owns the verdict; the
//! one thing added here is the verdict to report when eth_rpc cannot be read at all.

use serde_json::{json, Value};

/// The verdict for an eth_rpc that did not answer or answered a shape we cannot read.
/// Unknown blocks: defaulting to `off` here would be a false assurance.
pub fn unknown_verdict(chain_id: u64, why: &str) -> Value {
    json!({
        "ok": false, "error": why, "chainId": chain_id,
        "mode": "unknown", "state": "unhealthy", "usable": false, "blocking": true,
        "message": "The verified-proxy state could not be read.",
        "action": "restart_or_reload", "detail": why,
    })
}

/// A verdict is readable only when it carries BOTH a `state` and a boolean `blocking`.
pub fn readable(v: &Value) -> bool {
    v.get("state").and_then(Value::as_str).is_some() && v.get("blocking").and_then(Value::as_bool).is_some()
}

/// `raw` when it is readable, else a blocking unknown verdict carrying `raw`'s own error.
pub fn normalize(chain_id: u64, raw: &Value) -> Value {
    if readable(raw) {
        return raw.clone();
    }
    let why = raw.get("error").and_then(Value::as_str).unwrap_or("eth_rpc returned no usable verdict");
    unknown_verdict(chain_id, why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readable_verdict_passes_through_and_anything_else_blocks() {
        let ready = json!({ "ok": true, "chainId": 1, "mode": "required", "state": "ready", "blocking": false });
        assert_eq!(normalize(1, &ready), ready);
        for unreadable in [json!({}), json!({ "state": "ready" }), json!({ "blocking": false }),
                           json!({ "ok": false, "error": "boom" })] {
            let v = normalize(1, &unreadable);
            assert_eq!((v["mode"].as_str(), v["blocking"].as_bool(), v["chainId"].as_u64()),
                       (Some("unknown"), Some(true), Some(1)), "{unreadable}");
        }
        assert_eq!(normalize(1, &json!({ "ok": false, "error": "boom" }))["detail"], "boom");
    }
}
