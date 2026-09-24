//! The Uniswap app's rules, as pure functions over JSON: what a swap request means, what
//! `uniswap_module` and `tx_sender_module` are asked, what the human is told, and how the
//! sender's history groups into swaps. The glue makes the calls; it holds no rule of its own.

use std::cmp::Ordering;

use serde_json::{json, Value};

use crate::units::{add_base, compare_base, from_base_units, from_base_units_exact, is_digits, rate_of, to_base_units};

/// How this app tags the calls it asks the sender to make, so it finds its rows in a history
/// it shares with the wallet. Its own claim; who really asked is the sender's `origin`.
pub const APP: &str = "uniswap_ui";

pub const MAX_SLIPPAGE_BPS: i64 = 5_000;
/// One minute to three days, the bounds the Settings page offers.
pub const MAX_DEADLINE_MINS: i64 = 4_320;

/// A swap request. Tokens are an address, or the native coin as "ETH", "native" or nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwapForm {
    pub chain_id: i64,
    pub from: String,
    pub token_in: String,
    pub token_out: String,
    pub symbol_in: String,
    pub symbol_out: String,
    /// `None` until the caller or a token lookup names them.
    pub decimals_in: Option<u32>,
    pub decimals_out: Option<u32>,
    pub amount_units: String,
    /// Base units, once known.
    pub amount_in: String,
    pub tier: String,
    pub slippage_bps: i64,
    pub deadline_mins: i64,
    pub recipient: String,
    /// Fees the user set, wei in decimal digits. `None` leaves the field to the tier.
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
    /// One per call, in the order the swap is built; `None` leaves that call estimated.
    pub gas_limits: Vec<Option<String>>,
    /// A number to replace the transaction pending at. The sender refuses it for a bundle.
    pub nonce: Option<u64>,
}

fn text(o: &Value, key: &str) -> String {
    o.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn str_of<'a>(o: &'a Value, key: &str) -> &'a str {
    o.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The request, validated as far as it can be without asking any module. A request that is
/// not a swap is refused in words the user can act on, never sent on as zero.
pub fn parse_form(request_json: &str) -> Result<SwapForm, String> {
    let r: Value = serde_json::from_str(request_json).map_err(|e| format!("invalid swap request: {e}"))?;
    if !r.is_object() {
        return Err("a swap request must be a JSON object".into());
    }
    let decimals = |key: &str| r.get(key).and_then(Value::as_u64).and_then(|d| u32::try_from(d).ok());
    let tier = text(&r, "tier");
    let wei = |v: Option<&Value>, what: &str| -> Result<Option<String>, String> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(t)) if t.trim().is_empty() => Ok(None),
            Some(Value::String(t)) if is_digits(t.trim()) => Ok(Some(t.trim().to_string())),
            Some(Value::Number(n)) if n.is_u64() => Ok(Some(n.to_string())),
            _ => Err(format!("{what} must be a whole number in decimal digits")),
        }
    };
    let gas_limits = match r.get("gasLimits") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|g| match wei(Some(g), "a gas limit")? {
                Some(v) if v.trim_start_matches('0').is_empty() => Err("a gas limit must be more than zero".to_string()),
                v => Ok(v),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("gasLimits must list one gas limit, or null, per call".into()),
    };
    let nonce = match wei(r.get("nonce"), "nonce")? {
        Some(n) => Some(n.parse::<u64>().map_err(|_| "nonce is out of range".to_string())?),
        None => None,
    };
    let f = SwapForm {
        chain_id: r.get("chainId").and_then(Value::as_i64).unwrap_or(0),
        from: text(&r, "from"),
        token_in: text(&r, "tokenIn"),
        token_out: text(&r, "tokenOut"),
        symbol_in: text(&r, "symbolIn"),
        symbol_out: text(&r, "symbolOut"),
        decimals_in: decimals("decimalsIn"),
        decimals_out: decimals("decimalsOut"),
        amount_units: text(&r, "amountUnits"),
        amount_in: text(&r, "amountIn"),
        tier: if tier.is_empty() { "normal".into() } else { tier },
        slippage_bps: r.get("slippageBps").and_then(Value::as_i64).unwrap_or(50),
        deadline_mins: r.get("deadlineMins").and_then(Value::as_i64).unwrap_or(30),
        recipient: text(&r, "recipient"),
        max_fee_per_gas: wei(r.get("maxFeePerGas"), "maxFeePerGas")?,
        max_priority_fee_per_gas: wei(r.get("maxPriorityFeePerGas"), "maxPriorityFeePerGas")?,
        gas_limits,
        nonce,
    };
    if f.chain_id <= 0 {
        return Err("chainId is required".into());
    }
    if f.from.is_empty() {
        return Err("no account is selected".into());
    }
    if f.token_in.is_empty() || f.token_out.is_empty() {
        return Err("both tokens are required".into());
    }
    if same_token(&f.token_in, &f.token_out) {
        return Err("the two tokens are the same".into());
    }
    if !(0..=MAX_SLIPPAGE_BPS).contains(&f.slippage_bps) {
        return Err("slippage must be between 0 and 5000 basis points".into());
    }
    if !(1..=MAX_DEADLINE_MINS).contains(&f.deadline_mins) {
        return Err("the deadline must be between 1 minute and 3 days".into());
    }
    match (f.amount_units.is_empty(), f.amount_in.is_empty()) {
        (true, true) => return Err("the amount is missing".into()),
        (false, false) => return Err("give amountUnits or amountIn, not both".into()),
        (true, false) if !is_digits(&f.amount_in) => {
            return Err("amountIn must be base units in decimal digits".into())
        }
        _ => {}
    }
    Ok(f)
}

/// Settle the base-unit amount once the sell token's decimals are known.
pub fn scale(f: &mut SwapForm) -> Result<(), String> {
    if f.amount_in.is_empty() {
        let d = f.decimals_in.ok_or("the sell token's decimals are unknown")?;
        f.amount_in = to_base_units(&f.amount_units, d)
            .ok_or_else(|| format!("the amount must be a number with at most {d} decimal places"))?;
    }
    if f.amount_in.trim_start_matches('0').is_empty() {
        return Err("there is nothing to swap".into());
    }
    Ok(())
}

/// The native coin, as uniswap_module spells it.
pub fn is_native(token: &str) -> bool {
    let t = token.trim();
    t.is_empty()
        || t.eq_ignore_ascii_case("eth")
        || t.eq_ignore_ascii_case("native")
        || (t.len() == 42 && t[..2].eq_ignore_ascii_case("0x") && t[2..].bytes().all(|c| c == b'0'))
}

pub fn is_address(token: &str) -> bool {
    let t = token.trim();
    t.len() == 42 && t[..2].eq_ignore_ascii_case("0x") && t[2..].bytes().all(|c| c.is_ascii_hexdigit())
}

fn same_token(a: &str, b: &str) -> bool {
    (is_native(a) && is_native(b)) || a.eq_ignore_ascii_case(b)
}

/// What uniswap_module is asked. `owner` makes its batch read the balance and the allowance;
/// a deadline of zero is a quote, which asks for none.
pub fn swap_module_request(f: &SwapForm, deadline: u64) -> Value {
    let mut o = json!({
        "tokenIn": f.token_in, "tokenOut": f.token_out, "amountIn": f.amount_in, "owner": f.from,
        "symbolIn": f.symbol_in, "symbolOut": f.symbol_out, "slippageBps": f.slippage_bps,
    });
    if !f.recipient.is_empty() {
        o["recipient"] = json!(f.recipient);
    }
    if deadline > 0 {
        o["deadline"] = json!(deadline);
    }
    o
}

/// The sender's request for a built swap, with the user's own fees, gas limits and nonce. A leg
/// carries a gas limit only when the user set one: `fee_module` estimates the swap behind its
/// approval, and a limit invented here would only stand in its way.
pub fn sender_request(built: &Value, f: &SwapForm, via: &str, purpose: &str) -> Result<Value, String> {
    let route = built.get("route").cloned().unwrap_or_else(|| json!({}));
    let built_calls = built.get("calls").and_then(Value::as_array).cloned().unwrap_or_default();
    // Limits set for another build of the swap would land on the wrong calls.
    if !f.gas_limits.is_empty() && f.gas_limits.len() != built_calls.len() {
        return Err(format!(
            "gas limits were set for {} calls, but the swap is now {} — review it again",
            f.gas_limits.len(),
            built_calls.len()
        ));
    }
    let calls: Vec<Value> = built_calls
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut meta = json!({
                "app": APP, "via": via, "kind": str_of(c, "kind"),
                "tokenIn": f.token_in, "tokenOut": f.token_out,
                "symbolIn": f.symbol_in, "symbolOut": f.symbol_out,
                "decimalsIn": f.decimals_in, "decimalsOut": f.decimals_out,
                "amountIn": f.amount_in, "amountOut": str_of(built, "amountOut"),
                "amountOutMin": str_of(built, "amountOutMin"), "route": route,
            });
            // Absent means the account itself, which is where `received` then looks.
            if !f.recipient.is_empty() {
                meta["recipient"] = json!(f.recipient);
            }
            let mut call = json!({ "to": str_of(c, "to"), "value": str_of(c, "value"), "data": str_of(c, "data"),
                                   "label": str_of(c, "label"), "meta": meta });
            if let Some(Some(g)) = f.gas_limits.get(i) {
                call["gasLimit"] = json!(g);
            }
            call
        })
        .collect();
    let mut req = json!({ "chainId": f.chain_id, "from": f.from, "purpose": purpose, "calls": calls, "tier": f.tier });
    if let Some(v) = &f.max_fee_per_gas {
        req["maxFeePerGas"] = json!(v);
    }
    if let Some(v) = &f.max_priority_fee_per_gas {
        req["maxPriorityFeePerGas"] = json!(v);
    }
    if let Some(n) = f.nonce {
        req["nonce"] = json!(n);
    }
    Ok(req)
}

/// The sentence the keystore shows the human and the sender records: what leaves, what at
/// least comes back, and who asked this module. Every digit: "<0.00001" is not a claim.
pub fn swap_purpose(f: &SwapForm, built: &Value, via: &str) -> String {
    let amount = from_base_units_exact(&f.amount_in, f.decimals_in.unwrap_or(18)).unwrap_or_default();
    let min = from_base_units_exact(str_of(built, "amountOutMin"), f.decimals_out.unwrap_or(18)).unwrap_or_default();
    let name = |symbol: &str, token: &str| if symbol.is_empty() { token.to_string() } else { symbol.to_string() };
    let mut purpose = format!(
        "Swap {amount} {} for at least {min} {} on Uniswap",
        name(&f.symbol_in, &f.token_in),
        name(&f.symbol_out, &f.token_out)
    );
    if !via.is_empty() {
        purpose.push_str(&format!(", via {via}"));
    }
    purpose
}

/// The quote a view renders: the built swap, the sender's pricing of it under `fee` (`Null`
/// when the sender did not answer), and the display strings derived from base units.
pub fn merged_quote(built: &Value, fee: &Value, f: &SwapForm) -> Value {
    let (din, dout) = (f.decimals_in.unwrap_or(18), f.decimals_out.unwrap_or(18));
    let shown = |base: &str, d: u32| from_base_units(base, d).unwrap_or_default();
    let exact = |base: &str, d: u32| from_base_units_exact(base, d).unwrap_or_default();
    let (out, min) = (str_of(built, "amountOut"), str_of(built, "amountOutMin"));
    let mut q = built.clone();
    q["from"] = json!(f.from);
    q["amountInDisplay"] = json!(shown(&f.amount_in, din));
    q["amountOutDisplay"] = json!(shown(out, dout));
    q["amountOutExact"] = json!(exact(out, dout));
    q["amountOutMinDisplay"] = json!(shown(min, dout));
    q["amountOutMinExact"] = json!(exact(min, dout));
    q["rate"] = json!(rate_of(&f.amount_in, din, out, dout).unwrap_or_default());
    q["rateInverse"] = json!(rate_of(out, dout, &f.amount_in, din).unwrap_or_default());
    let balance = str_of(built, "balanceIn");
    if is_digits(balance) {
        q["balanceInDisplay"] = json!(shown(balance, din));
        q["insufficientBalance"] = json!(compare_base(balance, &f.amount_in) == Ordering::Less);
    }
    q["fee"] = match fee {
        Value::Object(o) if !o.is_empty() => fee.clone(),
        _ => json!({ "ok": false, "error": "the sender did not answer" }),
    };
    q
}

/// The status of a bundle from its legs. The worst leg wins: a swap whose approval landed and
/// whose swap reverted is a failed swap, not a half-confirmed one. A replaced leg was never
/// mined, so it decides the bundle only as its last leg, the swap itself, once nothing moves.
pub fn bundle_status(legs: &[Value]) -> &'static str {
    let (mut failed, mut pending, mut stalled, mut blocked) = (false, false, false, false);
    let replaced = legs.last().is_some_and(|r| str_of(r, "status") == "replaced");
    for r in legs {
        match str_of(r, "status") {
            "failed" => failed = true,
            "confirmed" | "replaced" => {}
            _ => pending = true,
        }
        stalled |= r.get("stalled").and_then(Value::as_bool) == Some(true);
        blocked |= r.get("verificationBlocked").and_then(Value::as_bool) == Some(true);
    }
    if failed {
        "failed"
    } else if blocked {
        "blocked"
    } else if stalled {
        "stalled"
    } else if pending {
        "pending"
    } else if replaced {
        "replaced"
    } else {
        "confirmed"
    }
}

/// What a settled swap delivered, off its swap leg's decoded receipt logs: the ERC-20 Transfers
/// of `tokenOut` to the recipient, or for ether out the EIP-7708 ether logs to it. `None` while
/// the swap is unsettled and when no such log was decoded (ether before Glamsterdam, or a sender
/// that predates those logs): an amount nobody measured is not shown as one.
pub fn received(legs: &[Value], meta: &Value) -> Option<String> {
    let leg = legs.iter().find(|r| r.get("meta").is_some_and(|m| str_of(m, "kind") == "swap"))?;
    if str_of(leg, "status") != "confirmed" {
        return None;
    }
    let account = str_of(leg, "from");
    let recipient = match str_of(meta, "recipient") {
        "" => account,
        r => r,
    };
    let token_out = str_of(meta, "tokenOut");
    if is_native(token_out) && recipient.eq_ignore_ascii_case(account) {
        // The sender totals the ether that reached the account over every log, cap or not.
        let total = str_of(leg, "nativeReceivedWei");
        return is_digits(total).then(|| total.to_string());
    }
    let list = if is_native(token_out) { "nativeTransfers" } else { "transfers" };
    // The sender caps each list; a sum over a cut list could be short, so it is not a sum.
    if leg.get(&format!("{list}More")).and_then(Value::as_u64).is_some_and(|n| n > 0) {
        return None;
    }
    let mut total: Option<String> = None;
    for t in leg.get(list).and_then(Value::as_array).into_iter().flatten() {
        let token = is_native(token_out) || str_of(t, "contract").eq_ignore_ascii_case(token_out);
        if token && str_of(t, "to").eq_ignore_ascii_case(recipient) {
            total = Some(add_base(total.as_deref().unwrap_or("0"), str_of(t, "amount"))?);
        }
    }
    total
}

/// This app's swaps out of the sender's rows: the rows it tagged, grouped by bundle, newest
/// first. A row with no request id is a bundle of its own, keyed by its hash.
pub fn group_swaps(rows: &[Value], app: &str) -> Vec<Value> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for r in rows {
        if r.get("meta").map(|m| str_of(m, "app")) != Some(app) {
            continue;
        }
        let mut id = str_of(r, "requestId").to_string();
        if id.is_empty() {
            id = str_of(r, "hash").to_string();
        }
        match groups.iter_mut().find(|(g, _)| *g == id) {
            Some((_, legs)) => legs.push(r.clone()),
            None => groups.push((id, vec![r.clone()])),
        }
    }
    let mut finished: Vec<Value> = groups
        .into_iter()
        .map(|(id, mut legs)| {
            legs.sort_by_key(|r| r.get("leg").and_then(Value::as_i64).unwrap_or(0));
            let hashes: Vec<&str> = legs.iter().map(|r| str_of(r, "hash")).filter(|h| !h.is_empty()).collect();
            let (mut swap, mut label) = (json!({}), String::new());
            for r in &legs {
                let meta = r.get("meta").cloned().unwrap_or_else(|| json!({}));
                if str_of(&meta, "kind") == "swap" || swap.as_object().is_some_and(|o| o.is_empty()) {
                    label = str_of(r, "label").to_string();
                    swap = meta;
                }
            }
            let newest = legs.iter().filter_map(|r| r.get("timestamp").and_then(Value::as_f64)).fold(0.0, f64::max);
            let first = legs.first().cloned().unwrap_or_default();
            let got = received(&legs, &swap);
            let mut g = json!({
                "requestId": id, "status": bundle_status(&legs), "timestamp": newest,
                "hashes": hashes, "swap": swap, "label": label,
                "origin": str_of(&first, "origin"),
                "via": first.get("meta").map(|m| str_of(m, "via")).unwrap_or(""),
                "legs": legs,
            });
            if let Some(base) = got {
                let decimals = g["swap"].get("decimalsOut").and_then(Value::as_u64).and_then(|d| u32::try_from(d).ok());
                if let Some(d) = decimals {
                    g["receivedDisplay"] = json!(from_base_units(&base, d).unwrap_or_default());
                    g["receivedExact"] = json!(from_base_units_exact(&base, d).unwrap_or_default());
                }
                g["received"] = json!(base);
            }
            g
        })
        .collect();
    let ts = |g: &Value| g.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
    finished.sort_by(|a, b| ts(b).partial_cmp(&ts(a)).unwrap_or(Ordering::Equal));
    finished
}

/// Whether a `send_status` reply is the end of the swap: the sender's own `final`. A sender that
/// predates it is read the way it behaves — `awaitingApproval` (held by the verified gate
/// included) and `broadcasting` still move — and none of its refusals is final, because it
/// cannot say which of them is.
pub fn is_final(reply: &Value) -> bool {
    if let Some(f) = reply.get("final").and_then(Value::as_bool) {
        return f;
    }
    reply.get("ok").and_then(Value::as_bool) == Some(true)
        && !matches!(str_of(reply, "status"), "awaitingApproval" | "broadcasting")
}

/// The chains uniswap_module holds a deployment for, from its `get_chains` reply.
pub fn deployed_chains(get_chains: &Value) -> Vec<u64> {
    get_chains
        .get("chains")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("chainId").and_then(Value::as_u64))
        .collect()
}

/// eth_rpc's enabled, in-scope records split by whether Uniswap is deployed there. A chain the
/// user enabled but Uniswap is not on is named, so a view can say why it is missing.
pub fn swappable_networks(records: &[Value], deployed: &[u64]) -> (Vec<Value>, Vec<u64>) {
    let (mut networks, mut unsupported) = (Vec::new(), Vec::new());
    for r in records {
        let flag = |k: &str| r.get(k).and_then(Value::as_bool) == Some(true);
        let Some(id) = r.get("chainId").and_then(Value::as_u64) else { continue };
        if !flag("enabled") || !flag("inScope") {
            continue;
        }
        if !deployed.contains(&id) {
            unsupported.push(id);
            continue;
        }
        let mut row = json!({ "chainId": id, "enabled": true, "inScope": true });
        for key in ["name", "nativeSymbol", "nativeDecimals", "testnet", "verifiedProxyMode"] {
            if let Some(v) = r.get(key) {
                row[key] = v.clone();
            }
        }
        networks.push(row);
    }
    (networks, unsupported)
}

/// A caller's token list: an array of objects, or nothing at all.
pub fn parse_tokens(tokens_json: &str) -> Result<Vec<Value>, String> {
    if tokens_json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(tokens_json).map_err(|e| format!("invalid token list: {e}"))?;
    let rows = v.as_array().ok_or("a token list must be a JSON array")?;
    if rows.iter().any(|r| !r.is_object()) {
        return Err("every token in the list must be an object".into());
    }
    Ok(rows.clone())
}

/// The offered rows, then any extra the caller named that they do not already hold.
pub fn union_tokens(offered: Vec<Value>, extra: Vec<Value>) -> Vec<Value> {
    let mut out = offered;
    for t in extra {
        let address = str_of(&t, "address");
        if !out.iter().any(|o| str_of(o, "address").eq_ignore_ascii_case(address)) {
            out.push(t);
        }
    }
    out
}

/// Whether the native row answers a picker query: symbol or name, case-insensitive substring.
pub fn native_matches(native: &Value, query: &str) -> bool {
    let needle = query.trim().to_ascii_lowercase();
    needle.is_empty()
        || str_of(native, "symbol").to_ascii_lowercase().contains(&needle)
        || str_of(native, "name").to_ascii_lowercase().contains(&needle)
}

/// token_list's offset for the picker's `offset`: the native row takes slot 0 of page 0.
pub fn provider_offset(native_matches: bool, offset: i64) -> i64 {
    let offset = offset.max(0);
    if native_matches { (offset - 1).max(0) } else { offset }
}

/// token_list's page with the native row merged in, counts included.
pub fn merge_native_page(native: &Value, native_matches: bool, mut page: Value, offset: i64, limit: i64) -> Value {
    let offset = usize::try_from(offset).unwrap_or(0);
    let provider_total = page.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut rows = page.get_mut("tokens").and_then(Value::as_array_mut).map(std::mem::take).unwrap_or_default();
    for row in &mut rows {
        row["native"] = json!(false);
    }
    if native_matches && offset == 0 && limit != 0 {
        rows.insert(0, native.clone());
    }
    if let Ok(cut) = usize::try_from(limit) {
        if cut > 0 {
            rows.truncate(cut);
        }
    }
    let total = provider_total + usize::from(native_matches);
    page["total"] = json!(total);
    page["offset"] = json!(offset);
    page["shown"] = json!(rows.len());
    page["hasMore"] = json!(offset.saturating_add(rows.len()) < total);
    page["tokens"] = json!(rows);
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
    const ROUTER: &str = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";

    fn form(extra: Value) -> SwapForm {
        let mut r = json!({ "chainId": 11155111, "from": "0xf39F", "tokenIn": USDC, "tokenOut": "ETH",
                            "amountUnits": "1000", "decimalsIn": 6, "decimalsOut": 18,
                            "symbolIn": "USDC", "symbolOut": "ETH", "slippageBps": 100 });
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        let mut f = parse_form(&r.to_string()).unwrap();
        scale(&mut f).unwrap();
        f
    }

    fn row(hash: &str, req: &str, leg: i64, status: &str, kind: &str, app: &str, ts: f64) -> Value {
        json!({ "hash": hash, "requestId": req, "leg": leg, "legs": 2, "status": status, "timestamp": ts,
                "label": format!("L{leg}"), "origin": "host", "to": "0x1",
                "meta": { "app": app, "via": "uniswap_ui", "kind": kind, "symbolIn": "USDC", "symbolOut": "ETH",
                          "amountIn": "1000000000", "decimalsIn": 6, "amountOut": "5", "decimalsOut": 18 } })
    }

    #[test]
    fn a_form_is_refused_in_words_rather_than_sent_on_as_zero() {
        let refused = |r: Value| parse_form(&r.to_string()).and_then(|mut f| scale(&mut f)).unwrap_err();
        let base = json!({ "chainId": 1, "from": "0xf39F", "tokenIn": "ETH", "tokenOut": USDC, "amountUnits": "1", "decimalsIn": 18 });
        let with = |k: &str, v: Value| { let mut r = base.clone(); r[k] = v; r };
        assert!(refused(with("amountUnits", json!("1.2345678901234567890123"))).contains("18 decimal places"));
        assert_eq!(refused(with("from", json!(""))), "no account is selected");
        assert!(refused(with("slippageBps", json!(6000))).contains("5000"));
        assert_eq!(refused(with("tokenOut", json!("native"))), "the two tokens are the same");
        assert_eq!(refused(with("chainId", json!(0))), "chainId is required");
        assert!(refused(with("deadlineMins", json!(0))).contains("deadline"));
        assert_eq!(refused(with("amountUnits", json!("0.0"))), "there is nothing to swap");
        assert!(refused(with("amountIn", json!("5"))).contains("not both"));
        assert!(refused(json!({ "chainId": 1, "from": "a", "tokenIn": "ETH", "tokenOut": USDC, "amountIn": "1e18" })).contains("decimal digits"));
    }

    #[test]
    fn a_good_form_scales_to_base_units_and_keeps_its_choices() {
        let f = form(json!({ "tier": "fast" }));
        assert_eq!(f.amount_in, "1000000000");
        assert_eq!(f.tier, "fast");
        assert_eq!(form(json!({})).tier, "normal");
        let mut unknown = parse_form(&json!({ "chainId": 1, "from": "a", "tokenIn": USDC, "tokenOut": "ETH", "amountUnits": "1" }).to_string()).unwrap();
        assert_eq!(unknown.decimals_in, None, "decimals are looked up, never assumed");
        assert!(scale(&mut unknown).unwrap_err().contains("decimals are unknown"));
    }

    #[test]
    fn the_module_is_asked_with_the_owner_and_only_a_swap_carries_a_deadline() {
        let f = form(json!({}));
        let m = swap_module_request(&f, 1_789_000_000);
        assert_eq!(m["owner"], "0xf39F");
        assert_eq!(m["amountIn"], "1000000000");
        assert_eq!(m["slippageBps"], 100);
        assert_eq!(m["deadline"], 1_789_000_000u64);
        assert!(swap_module_request(&f, 0).get("deadline").is_none(), "a quote asks for no deadline");
    }

    fn built() -> Value {
        json!({ "ok": true, "amountOut": "5", "amountOutMin": "4", "route": { "version": "V2" },
                "calls": [ { "kind": "approve", "to": USDC, "value": "0x0", "data": "0x09", "gasLimitHint": 60000, "label": "Approve USDC for Uniswap" },
                           { "kind": "swap", "to": ROUTER, "value": "0x0", "data": "0x18", "gasLimitHint": 180000, "label": "Swap USDC for ETH on Uniswap V2" } ] })
    }

    #[test]
    fn the_sender_gets_every_call_tagged_and_no_gas_limit() {
        let f = form(json!({}));
        let sr = sender_request(&built(), &f, "uniswap_ui", "P").unwrap();
        let calls = sr["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0]["label"].as_str().unwrap().starts_with("Approve"), "in order");
        assert!(calls.iter().all(|c| c.get("gasLimit").is_none()), "the estimator prices every leg");
        let meta = &calls[1]["meta"];
        assert_eq!(meta["app"], APP);
        assert_eq!(meta["via"], "uniswap_ui");
        assert_eq!(meta["amountOutMin"], "4");
        assert_eq!(meta["decimalsIn"], 6);
        assert_eq!(meta["route"]["version"], "V2");
        assert_eq!(sr["tier"], "normal");
        assert_eq!(sr["chainId"], 11155111);
        assert_eq!(sr["purpose"], "P");
        for k in ["maxFeePerGas", "maxPriorityFeePerGas", "nonce"] {
            assert!(sr.get(k).is_none(), "{k} is the sender's to choose unless the user set it");
        }
    }

    // The user's own fees, per-call gas limits and nonce reach the sender as given; the rules
    // for them (the tier, a replacement's floor, a bundle that cannot be pinned) are its own.
    #[test]
    fn the_users_fee_fields_reach_the_sender_as_given() {
        let f = form(json!({ "maxFeePerGas": "372524310", "maxPriorityFeePerGas": 37979581,
                             "gasLimits": [null, "200000"], "nonce": 40 }));
        let sr = sender_request(&built(), &f, "uniswap_ui", "P").unwrap();
        assert_eq!((sr["maxFeePerGas"].as_str(), sr["maxPriorityFeePerGas"].as_str()), (Some("372524310"), Some("37979581")));
        assert_eq!(sr["nonce"], 40);
        let calls = sr["calls"].as_array().unwrap();
        assert!(calls[0].get("gasLimit").is_none(), "an unset limit is left to the estimator");
        assert_eq!(calls[1]["gasLimit"], "200000");
        let blank = form(json!({ "maxFeePerGas": "", "gasLimits": [], "nonce": null }));
        assert_eq!((blank.max_fee_per_gas, blank.gas_limits.len(), blank.nonce), (None, 0, None));
    }

    #[test]
    fn a_fee_field_that_is_not_a_whole_number_is_refused() {
        let refused = |k: &str, v: Value| {
            let mut r = json!({ "chainId": 1, "from": "0xf39F", "tokenIn": "ETH", "tokenOut": USDC, "amountUnits": "1" });
            r[k] = v;
            parse_form(&r.to_string()).unwrap_err()
        };
        assert!(refused("maxFeePerGas", json!("3.5 gwei")).contains("maxFeePerGas must be a whole number"));
        assert!(refused("maxPriorityFeePerGas", json!(-1)).contains("maxPriorityFeePerGas"));
        assert!(refused("gasLimits", json!("200000")).contains("one gas limit, or null, per call"));
        assert!(refused("gasLimits", json!(["0"])).contains("more than zero"));
        assert!(refused("nonce", json!("forty")).contains("nonce must be a whole number"));
        assert!(refused("nonce", json!("99999999999999999999")).contains("out of range"));
    }

    // Limits set against one build of the swap must not land on another's calls.
    #[test]
    fn gas_limits_for_a_differently_built_swap_are_refused() {
        let f = form(json!({ "gasLimits": ["180000"] }));
        let e = sender_request(&built(), &f, "uniswap_ui", "P").unwrap_err();
        assert!(e.contains("set for 1 calls, but the swap is now 2"), "{e}");
    }

    #[test]
    fn the_purpose_names_every_digit_and_who_asked() {
        let f = form(json!({}));
        assert_eq!(swap_purpose(&f, &built(), ""), "Swap 1000 USDC for at least 0.000000000000000004 ETH on Uniswap");
        assert_eq!(swap_purpose(&f, &built(), "uniswap_ui"),
                   "Swap 1000 USDC for at least 0.000000000000000004 ETH on Uniswap, via uniswap_ui");
        let nameless = form(json!({ "symbolIn": "", "symbolOut": "" }));
        assert!(swap_purpose(&nameless, &built(), "host").starts_with(&format!("Swap 1000 {USDC} for")));
    }

    #[test]
    fn the_merged_quote_carries_displays_the_balance_verdict_and_the_fee() {
        let f = form(json!({}));
        let b = json!({ "ok": true, "chainId": 11155111, "owner": "0xf39F", "amountOut": "333277787035494084",
                        "amountOutMin": "331611398100316613", "balanceIn": "5000000000000" });
        let q = merged_quote(&b, &json!({ "ok": true, "feeCeilingWeiDisplay": "0.0004" }), &f);
        assert_eq!(q["from"], "0xf39F");
        assert_eq!(q["amountOutDisplay"], "0.33327");
        assert_eq!(q["amountOutMinDisplay"], "0.33161");
        assert_eq!(q["rate"], "0.000333278");
        assert_eq!(q["insufficientBalance"], false);
        assert_eq!(q["fee"]["feeCeilingWeiDisplay"], "0.0004");
        let short = json!({ "ok": true, "chainId": 11155111, "amountOut": "5", "amountOutMin": "4", "balanceIn": "1" });
        let nofee = merged_quote(&short, &Value::Null, &f);
        assert_eq!(nofee["ok"], true, "a sender that did not answer leaves the quote standing");
        assert_eq!(nofee["fee"]["ok"], false);
        assert_eq!(nofee["insufficientBalance"], true);
    }

    #[test]
    fn swaps_are_this_apps_rows_grouped_by_bundle_newest_first() {
        let rows = vec![
            row("0xa1", "snd_1", 1, "confirmed", "swap", APP, 200.0),
            row("0xa0", "snd_1", 0, "confirmed", "approve", APP, 199.0),
            row("0xb0", "snd_2", 0, "pending", "swap", APP, 300.0),
            json!({ "hash": "0xc0", "requestId": "", "leg": 0, "status": "confirmed", "timestamp": 400, "meta": { "kind": "native" } }),
        ];
        let groups = group_swaps(&rows, APP);
        assert_eq!(groups.len(), 2, "the wallet's own row is not one of ours");
        assert_eq!(groups[0]["requestId"], "snd_2");
        assert_eq!(groups[0]["status"], "pending");
        let b1 = &groups[1];
        assert_eq!(b1["requestId"], "snd_1");
        assert_eq!(b1["legs"][0]["leg"], 0, "legs in order");
        assert_eq!(b1["label"], "L1", "titled by the swap leg");
        assert_eq!(b1["swap"]["kind"], "swap");
        assert_eq!(b1["hashes"].as_array().unwrap().len(), 2);
        assert_eq!(b1["status"], "confirmed");
        assert_eq!(b1["origin"], "host");
        assert_eq!(b1["via"], "uniswap_ui");
    }

    const ME: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
    const WETH: &str = "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14";

    /// The confirmed swap leg of a bundle, as the sender returns it after reading its receipt.
    fn settled(token_out: &str, extra: Value) -> Vec<Value> {
        let mut swap = row("0xa1", "snd_1", 1, "confirmed", "swap", APP, 2.0);
        swap["from"] = json!(ME);
        swap["meta"]["tokenOut"] = json!(token_out);
        for (k, v) in extra.as_object().unwrap() {
            swap[k] = v.clone();
        }
        vec![row("0xa0", "snd_1", 0, "confirmed", "approve", APP, 1.0), swap]
    }

    /// Quoted and minimum are what was EXPECTED; the receipt says what arrived. A token comes
    /// in as an ERC-20 Transfer to the account, and only transfers of the token bought count.
    #[test]
    fn a_settled_swap_reads_what_it_received_off_its_receipt() {
        let legs = settled(WETH, json!({ "transfers": [
            { "contract": USDC, "from": ME, "to": ROUTER, "amount": "1000000000" },
            { "contract": WETH, "from": ROUTER, "to": ME.to_lowercase(), "amount": "333000000000000000" },
            { "contract": WETH, "from": ROUTER, "to": ROUTER, "amount": "7" },
            { "contract": USDC, "from": ROUTER, "to": ME, "amount": "1" }] }));
        assert_eq!(received(&legs, &legs[1]["meta"]).as_deref(), Some("333000000000000000"),
                   "the refund of the token sold is not what was bought");
        let g = &group_swaps(&legs, APP)[0];
        assert_eq!((g["received"].clone(), g["receivedDisplay"].clone(), g["receivedExact"].clone()),
                   (json!("333000000000000000"), json!("0.333"), json!("0.333")));
    }

    /// EIP-7708: ether out arrives as the router's CALL to the account, which the sender now
    /// decodes and totals. Before Glamsterdam no log says so, and nothing is claimed.
    #[test]
    fn ether_out_is_read_off_eip7708_logs_and_unmeasured_ether_claims_nothing() {
        let legs = settled("ETH", json!({ "nativeReceivedWei": "333277787035494084" }));
        let g = &group_swaps(&legs, APP)[0];
        assert_eq!((g["received"].clone(), g["receivedDisplay"].clone()),
                   (json!("333277787035494084"), json!("0.33327")));

        let before = settled("ETH", json!({}));
        assert!(group_swaps(&before, APP)[0].get("received").is_none(), "no log, no figure");

        // A sender that predates EIP-7708 handling files the log as a "token" of 0xff…fe. It is
        // not the token bought, so it is never read as one.
        let legacy = settled(WETH, json!({ "transfers": [
            { "contract": "0xfffffffffffffffffffffffffffffffffffffffe", "from": ROUTER, "to": ME,
              "amount": "5" }] }));
        assert_eq!(received(&legacy, &legacy[1]["meta"]), None);
    }

    /// A recipient other than the account is recorded with the call and read back from there:
    /// its ether is in the transfer list, not in the account's own total.
    #[test]
    fn what_reached_another_recipient_is_summed_from_its_transfers() {
        const THEM: &str = "0x0adBc7B2D1A2b7C8E9F0A1b2c3d4e5f60718D3A7";
        let mut legs = settled("ETH", json!({ "nativeReceivedWei": "999", "nativeTransfers": [
            { "from": WETH, "to": ROUTER, "amount": "5" },
            { "from": ROUTER, "to": THEM, "amount": "3" },
            { "from": ROUTER, "to": THEM.to_lowercase(), "amount": "2" }] }));
        legs[1]["meta"]["recipient"] = json!(THEM);
        assert_eq!(received(&legs, &legs[1]["meta"]).as_deref(), Some("5"));

        let f = form(json!({ "recipient": THEM }));
        let sr = sender_request(&built(), &f, "uniswap_ui", "P").unwrap();
        assert_eq!(sr["calls"][1]["meta"]["recipient"], THEM);
        let own = sender_request(&built(), &form(json!({})), "uniswap_ui", "P").unwrap();
        assert!(own["calls"][1]["meta"].get("recipient").is_none(), "absent means the account");
    }

    #[test]
    fn a_list_the_sender_cut_short_is_not_summed() {
        let mut legs = settled(WETH, json!({ "transfersMore": 3, "transfers": [
            { "contract": WETH, "from": ROUTER, "to": ME, "amount": "5" }] }));
        assert_eq!(received(&legs, &legs[1]["meta"]), None, "three more transfers went unread");
        legs[1]["transfersMore"] = json!(null);
        assert_eq!(received(&legs, &legs[1]["meta"]).as_deref(), Some("5"));
    }

    #[test]
    fn an_unsettled_swap_has_received_nothing_yet() {
        let mut legs = settled("ETH", json!({ "nativeReceivedWei": "5" }));
        legs[1]["status"] = json!("pending");
        assert_eq!(received(&legs, &legs[1]["meta"]), None);
        legs[1]["status"] = json!("failed");
        assert_eq!(received(&legs, &legs[1]["meta"]), None, "a reverted swap delivered nothing");
    }

    #[test]
    fn a_bundle_is_as_bad_as_its_worst_leg() {
        let failed = [row("0x1", "r", 0, "confirmed", "approve", APP, 1.0), row("0x2", "r", 1, "failed", "swap", APP, 2.0)];
        assert_eq!(bundle_status(&failed), "failed");
        let mut stalled = row("0x3", "r", 0, "pending", "swap", APP, 3.0);
        stalled["stalled"] = json!(true);
        assert_eq!(bundle_status(&[stalled.clone()]), "stalled");
        stalled["verificationBlocked"] = json!(true);
        assert_eq!(bundle_status(&[stalled]), "blocked");
    }

    #[test]
    fn a_replaced_swap_is_replaced_and_a_replaced_approval_is_not_the_swap() {
        let replaced = row("0x4", "r", 0, "replaced", "swap", APP, 4.0);
        assert_eq!(bundle_status(&[replaced.clone()]), "replaced");
        let approval_replaced = [row("0x5", "r", 0, "replaced", "approve", APP, 5.0),
                                 row("0x6", "r", 1, "confirmed", "swap", APP, 6.0)];
        assert_eq!(bundle_status(&approval_replaced), "confirmed", "the swap itself landed");
        let still_moving = [row("0x7", "r", 0, "replaced", "approve", APP, 7.0),
                            row("0x8", "r", 1, "pending", "swap", APP, 8.0)];
        assert_eq!(bundle_status(&still_moving), "pending");
        let reverted = [row("0x9", "r", 0, "replaced", "approve", APP, 9.0),
                        row("0xa", "r", 1, "failed", "swap", APP, 10.0)];
        assert_eq!(bundle_status(&reverted), "failed");
    }

    #[test]
    fn only_the_end_of_a_swap_is_final() {
        for (reply, want) in [
            (json!({ "ok": true, "status": "awaitingApproval", "final": false }), false),
            (json!({ "ok": true, "status": "awaitingApproval", "blocked": true, "final": false }), false),
            (json!({ "ok": true, "status": "broadcasting", "final": false }), false),
            (json!({ "ok": true, "status": "stuck", "final": true }), true),
            (json!({ "ok": true, "status": "broadcast", "final": true }), true),
            (json!({ "ok": false, "error": "no time left to read the approval", "final": false }), false),
            (json!({ "ok": false, "error": "no send with id 'snd_x'", "final": true }), true),
        ] {
            assert_eq!(is_final(&reply), want, "{reply}");
        }
        // The sender's word, never its sentence.
        assert!(!is_final(&json!({ "ok": false, "error": "no send with id 'snd_x'", "final": false })));
    }

    #[test]
    fn a_sender_that_predates_final_is_read_the_way_it_behaves() {
        for live in ["awaitingApproval", "broadcasting"] {
            assert!(!is_final(&json!({ "ok": true, "status": live })), "{live}");
        }
        assert!(!is_final(&json!({ "ok": true, "status": "awaitingApproval", "blocked": true })));
        for done in ["broadcast", "rejected", "cancelled", "failed", "stuck"] {
            assert!(is_final(&json!({ "ok": true, "status": done })), "{done}");
        }
        for error in ["no time left to read the approval", "no send with id 'snd_x'"] {
            assert!(!is_final(&json!({ "ok": false, "error": error })), "{error}");
        }
    }

    #[test]
    fn only_enabled_in_scope_chains_with_a_deployment_are_networks() {
        let records = vec![
            json!({ "chainId": 11155111, "name": "Sepolia", "enabled": true, "inScope": true, "testnet": true, "endpoint": "e" }),
            json!({ "chainId": 1, "name": "Ethereum", "enabled": true, "inScope": true, "testnet": false }),
            json!({ "chainId": 560048, "name": "Hoodi", "enabled": true, "inScope": true, "testnet": true }),
            json!({ "chainId": 10, "name": "Optimism", "enabled": true, "inScope": false }),
            json!({ "chainId": 8453, "name": "Base", "enabled": false, "inScope": false }),
        ];
        let (networks, unsupported) = swappable_networks(&records, &[1, 10, 8453, 11155111]);
        let ids: Vec<u64> = networks.iter().map(|n| n["chainId"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![11155111, 1]);
        assert_eq!(unsupported, vec![560048], "Hoodi is enabled but Uniswap is not there");
        assert!(networks[0].get("endpoint").is_none(), "the view gets names, not endpoints");
        assert_eq!(deployed_chains(&json!({ "ok": true, "chains": [{ "chainId": 1 }, { "chainId": 10 }] })), vec![1, 10]);
    }

    #[test]
    fn a_callers_extra_tokens_join_the_offered_ones_once() {
        let offered = vec![json!({ "address": USDC, "symbol": "USDC" })];
        let extra = parse_tokens(&json!([{ "address": USDC.to_lowercase(), "symbol": "X" }, { "address": ROUTER, "symbol": "R" }]).to_string()).unwrap();
        let all = union_tokens(offered, extra);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["symbol"], "USDC", "the offered row wins");
        assert!(parse_tokens("").unwrap().is_empty());
        assert!(parse_tokens("{}").is_err());
        assert!(parse_tokens("[1]").is_err());
    }

    #[test]
    fn the_native_row_takes_slot_zero_of_the_first_page_only() {
        let native = json!({ "symbol": "ETH", "name": "Ether", "native": true });
        assert!(native_matches(&native, "et"));
        assert!(!native_matches(&native, "usd"));
        assert_eq!(provider_offset(true, 0), 0);
        assert_eq!(provider_offset(true, 100), 99);
        assert_eq!(provider_offset(false, 100), 100);
        let page = json!({ "ok": true, "chainId": 1, "total": 3, "listed": 3, "tokens": [{ "symbol": "A" }, { "symbol": "B" }, { "symbol": "C" }] });
        let first = merge_native_page(&native, true, page.clone(), 0, 3);
        let symbols: Vec<&str> = first["tokens"].as_array().unwrap().iter().map(|t| t["symbol"].as_str().unwrap()).collect();
        assert_eq!(symbols, vec!["ETH", "A", "B"]);
        assert_eq!((first["total"].as_u64(), first["shown"].as_u64(), first["hasMore"].as_bool()), (Some(4), Some(3), Some(true)));
        assert_eq!(first["tokens"][1]["native"], false);
        let later = merge_native_page(&native, true, json!({ "total": 3, "tokens": [{ "symbol": "C" }] }), 3, 3);
        assert_eq!(later["tokens"][0]["symbol"], "C");
        assert_eq!(later["hasMore"], false);
    }
}
