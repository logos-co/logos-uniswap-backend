//! Logos module glue for `uniswap_backend`.
//!
//! The builder derives the `.lidl` from the `UniswapBackendModule` trait below. Compiled only
//! with the default `logos_module` feature; the rules it applies live in `app.rs`.
//!
//! `concurrency: "multi"`: every method blocks on outbound calls, so one slow quote cannot stall
//! a status poll. The module keeps no state beyond its relay flags, so it takes no lock at all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::app::{self, SwapForm};
use crate::budget::{
    callee_deadline, Budget, ACCOUNTS_BUDGET, ASSETS_BUDGET, BALANCES_BUDGET, BUILD_BUDGET,
    CANCEL_BUDGET, FEES_BUDGET, HISTORY_BUDGET, INIT_BUDGET, LOCAL_BUDGET, PROBE_BUDGET,
    QUOTE_BUDGET, READ_BUDGET, SENDER_BUDGET, STARTUP_BUDGET, STATUS_BUDGET, SWAP_BUDGET,
    TOKENS_BUDGET, VERDICT_BUDGET,
};
use crate::depinit;
use crate::verified;

pub trait UniswapBackendModule: Send + Sync + 'static {
    /// The chains a swap can happen on: eth_rpc's enabled, in-scope chains that uniswap_module
    /// holds a deployment for. `{ ok, scope, networks: [{ chainId, name, nativeSymbol,
    /// nativeDecimals, testnet, verifiedProxyMode }], unsupported: [chainId] }`; `unsupported`
    /// names enabled chains Uniswap is not deployed on.
    fn networks(&self) -> String;

    /// eth_rpc's verified-proxy verdict for `chain_id`: `{ ok, chainId, mode, state, usable,
    /// blocking, message, action, detail }`. A verdict that cannot be read is a blocking
    /// `mode: "unknown"` one, never `off`.
    fn verdict(&self, chain_id: i64) -> String;

    /// The native coin, then the tokens offered on `chain_id` (pinned and enabled), as asset
    /// rows: `{ ok, chainId, tokens: [{ symbol, name, decimals, address?, native, … }] }`.
    fn tokens(&self, chain_id: i64) -> String;

    /// The token picker: token_list's catalogue for `chain_id`, the native row first on the
    /// first page. `{ ok, chainId, total, offset, shown, hasMore, listed, tokens, listError? }`.
    fn catalogue(&self, chain_id: i64, query: String, offset: i64, limit: i64) -> String;

    /// Balances of `address` on `chain_id` for the native coin, every offered token and any
    /// extra token rows the caller names (the pair on screen, say). evm_assets' reply:
    /// `{ ok, chainId, address, tokenSort, balances, route }`.
    fn balances(&self, chain_id: i64, address: String, tokens_json: String) -> String;

    /// fee_module's tiers for `chain_id`, relayed verbatim.
    fn fee_tiers(&self, chain_id: i64) -> String;

    /// The keystore's accounts, their names and the wallets they were derived under, in one
    /// reply: `{ ok, accounts, labels?, wallets? }`. Read-only: this module can never create,
    /// import or sign.
    fn accounts(&self) -> String;

    /// Price a swap without side effects. `request_json`: `{ chainId, from, tokenIn, tokenOut,
    /// amountUnits | amountIn, decimalsIn?, decimalsOut?, symbolIn?, symbolOut?, slippageBps?,
    /// deadlineMins?, tier?, recipient?, maxFeePerGas?, maxPriorityFeePerGas?, gasLimits?,
    /// nonce? }`; a token is an address, "ETH", or an offered symbol, and missing decimals are
    /// looked up. The fee fields are the user's, in wei, relayed as given, `gasLimits` one per
    /// call in build order. The reply is uniswap_module's `build_swap` with the display figures
    /// and `tx_sender_module`'s `prepare` under `fee`.
    fn quote(&self, request_json: String) -> String;

    /// Build the swap afresh and ask the sender to make it: `{ ok, pending, requestId, handle,
    /// chainId, from, purpose, amountOutMin, deadline }`. Nothing is signed yet; poll
    /// `swap_status`.
    fn swap(&self, request_json: String) -> String;

    /// Advance a swap and report where it got to: the sender's `send_status` and its `final`,
    /// read off `status` for a sender that predates it. Polling IS the broadcast. `final:
    /// false` means ask again, refusals included.
    fn swap_status(&self, request_id: String) -> String;

    /// Withdraw a swap nobody has approved yet, releasing its nonces.
    fn cancel_swap(&self, request_id: String) -> String;

    /// This app's swaps for `address` on `chain_id`, newest first, one entry per bundle:
    /// `{ ok, chainId, address, stillDue, swaps: [{ requestId, status, timestamp, hashes, legs,
    /// swap, label, origin, via }] }`. Receipts due are swept first.
    fn swaps(&self, address: String, chain_id: i64) -> String;

    fn on_context_ready(&self, _ctx: &RustModuleContext) {}
}

pub trait UniswapBackendModuleEvents {
    /// A chain's record, its enabled switch, or the device scope moved (`-1`: every chain).
    fn networks_changed(&self, chain_id: i64);
    /// The tokens offered on `chain_id`, or its native coin's metadata, moved.
    fn tokens_changed(&self, chain_id: i64);
    /// The keystore's accounts moved. `count` is advisory; re-read `accounts`.
    fn accounts_changed(&self, count: i64);
    /// A swap changed state.
    fn swap_status_changed(&self, request_id: String);
    /// A recorded row for `address` changed; empty when the sender named only a hash.
    fn swaps_changed(&self, address: String);
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated/provider_gen.rs"));

#[derive(Default)]
struct UniswapBackendImpl {
    eth_rpc_settled: AtomicBool,
    token_list_settled: AtomicBool,
    feeds: Feeds,
}

/// One flag per subscription, held while its listener runs, so a feed that ends re-arms on the
/// next read rather than going quiet for the life of the process.
#[derive(Default)]
struct Feeds {
    chains: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    scope: Arc<AtomicBool>,
    tokens: Arc<AtomicBool>,
    accounts: Arc<AtomicBool>,
    sends: Arc<AtomicBool>,
    txs: Arc<AtomicBool>,
    rows: Arc<AtomicBool>,
}

/// Arm one subscription: `on` subscribes, `body` drains it on a thread of its own. The flag is
/// released when the feed ends, or at once when it could not be armed.
fn feed<S: Send + 'static>(
    flag: &Arc<AtomicBool>,
    on: impl FnOnce() -> Option<S>,
    body: impl FnOnce(S) + Send + 'static,
) {
    if flag.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(sub) = on() else {
        flag.store(false, Ordering::SeqCst);
        return;
    };
    let flag = flag.clone();
    std::thread::spawn(move || {
        body(sub);
        flag.store(false, Ordering::SeqCst);
    });
}

/// A refusal as an object: one a dependency already worded passes through whole, verdicts
/// included; anything else is wrapped.
fn refusal(e: impl std::fmt::Display) -> Value {
    let message = e.to_string();
    match serde_json::from_str::<Value>(&message) {
        Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(false) => v,
        _ => json!({ "ok": false, "error": message }),
    }
}

fn err(e: impl std::fmt::Display) -> String {
    refusal(e).to_string()
}

/// A dependency's reply, parsed, with its refusal kept as the object it was. A transport error
/// names the module, so a module that is down reads differently from one that said no.
fn reply(raw: Result<String, impl std::fmt::Debug>, who: &str) -> Result<Value, String> {
    let raw = raw.map_err(|e| format!("{who}: {e:?}"))?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| format!("{who}: {e}"))?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(v.to_string());
    }
    Ok(v)
}

/// A reply relayed verbatim, refusals included.
fn relay(raw: Result<String, impl std::fmt::Debug>, who: &str) -> String {
    match raw {
        Ok(reply) => reply,
        Err(e) => err(format!("{who}: {e:?}")),
    }
}

/// The module that called this one, as the runtime attested it. Named on the swap and in the
/// purpose the human reads; never trusted for anything else.
fn caller_name() -> String {
    match logos_rust_sdk::current_caller() {
        logos_rust_sdk::LogosCaller::Module { name, .. } => name,
        logos_rust_sdk::LogosCaller::HostAnchor => "host".to_string(),
        logos_rust_sdk::LogosCaller::Derived { parent, leaf } => format!("{parent}/{leaf}"),
        logos_rust_sdk::LogosCaller::Operator { name } => format!("operator:{name}"),
        logos_rust_sdk::LogosCaller::Unknown => String::new(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn rows_of(v: &Value) -> Vec<Value> {
    v.get("tokens").and_then(Value::as_array).cloned().unwrap_or_default()
}

impl UniswapBackendImpl {
    /// Every relay this module keeps. Cheap once armed; the reads call it too, so a feed that
    /// could not be armed at startup, or that ended, comes back.
    fn arm(&self) {
        let f = &self.feeds;
        feed(&f.chains, || modules().eth_rpc_module.on_chain_config_changed().ok(), |sub| {
            for ev in sub {
                if let Some(e) = eth_rpc_module::EthRpcModuleClient::decode_chain_config_changed(&ev) {
                    // The record carries the native coin's metadata, which the token rows show.
                    emit_networks_changed(e.chain_id);
                    emit_tokens_changed(e.chain_id);
                }
            }
        });
        feed(&f.enabled, || modules().eth_rpc_module.on_chain_enabled_changed().ok(), |sub| {
            for ev in sub {
                if let Some(e) = eth_rpc_module::EthRpcModuleClient::decode_chain_enabled_changed(&ev) {
                    emit_networks_changed(e.chain_id);
                }
            }
        });
        feed(&f.scope, || modules().eth_rpc_module.on_network_scope_changed().ok(), |sub| {
            for ev in sub {
                if eth_rpc_module::EthRpcModuleClient::decode_network_scope_changed(&ev).is_some() {
                    emit_networks_changed(-1);
                }
            }
        });
        feed(&f.tokens, || modules().token_list_module.on_tokens_updated().ok(), |sub| {
            for ev in sub {
                if let Some(e) = token_list_module::TokenListModuleClient::decode_tokens_updated(&ev) {
                    emit_tokens_changed(e.chain_id);
                }
            }
        });
        feed(&f.accounts, || modules().keystore_module.on_accounts_changed().ok(), |sub| {
            // Arming is not retroactive: one announcement closes the window before it.
            emit_accounts_changed(-1);
            for ev in sub {
                let count = keystore_module::KeystoreModuleClient::decode_accounts_changed(&ev)
                    .map(|e| e.count)
                    .unwrap_or(-1);
                emit_accounts_changed(count);
            }
        });
        feed(&f.sends, || modules().tx_sender_module.on_send_status_changed().ok(), |sub| {
            for ev in sub {
                if let Some(e) = tx_sender_module::TxSenderModuleClient::decode_send_status_changed(&ev) {
                    emit_swap_status_changed(&e.request_id);
                }
            }
        });
        feed(&f.txs, || modules().tx_sender_module.on_tx_status_changed().ok(), |sub| {
            for ev in sub {
                if tx_sender_module::TxSenderModuleClient::decode_tx_status_changed(&ev).is_some() {
                    emit_swaps_changed("");
                }
            }
        });
        feed(&f.rows, || modules().tx_sender_module.on_history_changed().ok(), |sub| {
            for ev in sub {
                if let Some(e) = tx_sender_module::TxSenderModuleClient::decode_history_changed(&ev) {
                    emit_swaps_changed(&e.address);
                }
            }
        });
    }

    /// Have eth_rpc seed its default chains. No `config_status` gate: it fills only what is
    /// absent and seeds a default chain at most once per device, so a store another app has
    /// already written to still gets the defaults it lacks.
    fn ensure_eth_rpc(&self, b: &Budget) {
        if self.eth_rpc_settled.load(Ordering::Relaxed) {
            return;
        }
        let Some(t) = b.take(INIT_BUDGET) else { return };
        let applied = modules().eth_rpc_module.init_defaults_with_timeout(t);
        if applied.map(|raw| depinit::reply_ok(&raw)).unwrap_or(false) {
            self.eth_rpc_settled.store(true, Ordering::Relaxed);
        }
    }

    /// Have token_list apply its own defaults: a device with no wallet on it still needs a
    /// catalogue. No `config_status` gate: it writes them only when nothing is configured.
    fn ensure_token_list(&self, b: &Budget) {
        if self.token_list_settled.load(Ordering::Relaxed) {
            return;
        }
        let Some(t) = b.take(INIT_BUDGET) else { return };
        let applied = modules().token_list_module.init_defaults_with_timeout(t);
        if applied.map(|raw| depinit::reply_ok(&raw)).unwrap_or(false) {
            self.token_list_settled.store(true, Ordering::Relaxed);
        }
    }

    /// The chain registry, behind the lazy eth_rpc seeding retry.
    fn chain_configs(&self, b: &Budget) -> Result<(String, Vec<Value>), String> {
        self.ensure_eth_rpc(b);
        let t = b.take(PROBE_BUDGET).ok_or("no time left to read the chain registry")?;
        let v = reply(modules().eth_rpc_module.list_chain_configs_with_timeout(t), "eth_rpc_module")?;
        let scope = v.get("scope").and_then(Value::as_str).unwrap_or("mainnets").to_string();
        Ok((scope, v.get("chains").and_then(Value::as_array).cloned().unwrap_or_default()))
    }

    fn in_scope(&self, chain_id: i64, b: &Budget) -> Result<(), String> {
        let (_, records) = self.chain_configs(b)?;
        let on = |r: &Value, k: &str| r.get(k).and_then(Value::as_bool) == Some(true);
        records
            .iter()
            .any(|r| r.get("chainId").and_then(Value::as_i64) == Some(chain_id) && on(r, "enabled") && on(r, "inScope"))
            .then_some(())
            .ok_or_else(|| format!("chain {chain_id} is not enabled in the current network scope"))
    }

    /// The ERC-20 rows token_list offers on `chain_id`: pinned, then enabled.
    fn offered(&self, chain_id: i64, b: &Budget) -> Result<Vec<Value>, String> {
        let t = b.take(LOCAL_BUDGET).ok_or("no time left to read the offered tokens")?;
        reply(modules().token_list_module.list_offered_with_timeout(chain_id, t), "token_list_module").map(|v| rows_of(&v))
    }

    /// `tokens` as evm_assets' asset rows, the native coin first.
    fn assets(&self, chain_id: i64, tokens: &[Value], b: &Budget) -> Result<Value, String> {
        let t = b.take(LOCAL_BUDGET).ok_or("no time left to read the asset rows")?;
        reply(
            modules().evm_assets_module.list_assets_with_timeout(chain_id, &json!(tokens).to_string(), t),
            "evm_assets_module",
        )
    }

    fn verdict_of(chain_id: u64, raw: Result<String, impl std::fmt::Debug>) -> Value {
        match raw.map_err(|e| format!("{e:?}")).and_then(|r| serde_json::from_str::<Value>(&r).map_err(|e| e.to_string())) {
            Ok(v) => verified::normalize(chain_id, &v),
            Err(e) => verified::unknown_verdict(chain_id, &format!("eth_rpc_module: {e}")),
        }
    }

    /// Name one side of the swap exactly: its address (or "ETH"), decimals and symbol. A caller
    /// that names decimals is taken at its word, as the view always was; otherwise the token is
    /// looked up: an offered symbol or address first, then any address token_list holds.
    fn settle_side(&self, chain_id: i64, token: &mut String, symbol: &mut String, decimals: &mut Option<u32>, b: &Budget) -> Result<(), String> {
        if decimals.is_some() && (app::is_native(token) || app::is_address(token)) {
            return Ok(());
        }
        let offered = self.offered(chain_id, b)?;
        let t = b.take(LOCAL_BUDGET).ok_or("no time left to look the token up")?;
        let found = reply(
            modules().evm_assets_module.resolve_asset_with_timeout(chain_id, token, &json!(offered).to_string(), t),
            "evm_assets_module",
        )
        .map(|v| v.get("asset").cloned().unwrap_or_default());
        let asset = match found {
            Ok(a) => a,
            Err(_) if app::is_address(token) => self
                .listed(chain_id, token, b)?
                .ok_or_else(|| format!("{token} is not in the token list; name its decimals"))?,
            Err(refused) => return Err(refused),
        };
        let native = asset.get("native").and_then(Value::as_bool) == Some(true);
        *token = if native { "ETH".into() } else { asset.get("address").and_then(Value::as_str).unwrap_or(token).to_string() };
        if symbol.is_empty() {
            *symbol = asset.get("symbol").and_then(Value::as_str).unwrap_or("").to_string();
        }
        *decimals = asset.get("decimals").and_then(Value::as_u64).and_then(|d| u32::try_from(d).ok());
        decimals.map(|_| ()).ok_or_else(|| format!("{token} has no decimals on record"))
    }

    /// Any catalogue row token_list holds for `address` on `chain_id`, offered or not.
    fn listed(&self, chain_id: i64, address: &str, b: &Budget) -> Result<Option<Value>, String> {
        let t = b.take(LOCAL_BUDGET).ok_or("no time left to look the token up")?;
        let v = reply(
            modules().token_list_module.get_tokens_by_address_with_timeout(chain_id, &json!([address]).to_string(), t),
            "token_list_module",
        )?;
        Ok(rows_of(&v).into_iter().next())
    }

    /// A parsed request completed: both sides named exactly, the amount in base units.
    fn complete(&self, mut f: SwapForm, b: &Budget) -> Result<SwapForm, String> {
        let chain = f.chain_id;
        self.settle_side(chain, &mut f.token_in, &mut f.symbol_in, &mut f.decimals_in, b)?;
        self.settle_side(chain, &mut f.token_out, &mut f.symbol_out, &mut f.decimals_out, b)?;
        app::scale(&mut f)?;
        Ok(f)
    }

    /// uniswap_module's `build_swap`: the quote, and the calls in the order they must land.
    fn build(&self, f: &SwapForm, deadline: u64, b: &Budget) -> Result<Value, String> {
        let t = b.take(BUILD_BUDGET).ok_or("no time left to quote the swap")?;
        reply(
            modules().uniswap_module.build_swap_with_timeout(f.chain_id, &app::swap_module_request(f, deadline).to_string(), t),
            "uniswap_module",
        )
    }

    /// The sender's request for `built`, bounded by what is left of `b`. `Ok(None)` is a
    /// budget spent; `Err` is a request the user's own fields made impossible.
    fn sender_request(built: &Value, f: &SwapForm, via: &str, purpose: &str, b: &Budget)
        -> Result<Option<(Value, std::time::Duration)>, String> {
        let mut req = app::sender_request(built, f, via, purpose)?;
        let Some(t) = b.take(SENDER_BUDGET) else { return Ok(None) };
        if let Some(d) = callee_deadline(t) {
            req["deadlineMs"] = json!(d);
        }
        Ok(Some((req, t)))
    }
}

impl UniswapBackendModule for UniswapBackendImpl {
    fn on_context_ready(&self, _ctx: &RustModuleContext) {
        let b = Budget::new(STARTUP_BUDGET);
        self.ensure_eth_rpc(&b);
        self.ensure_token_list(&b);
        self.arm();
    }

    fn networks(&self) -> String {
        self.arm();
        let b = Budget::new(READ_BUDGET);
        let (scope, records) = match self.chain_configs(&b) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let Some(t) = b.take(PROBE_BUDGET) else { return err("no time left to read the Uniswap deployments") };
        let deployed = match reply(modules().uniswap_module.get_chains_with_timeout(t), "uniswap_module") {
            Ok(v) => app::deployed_chains(&v),
            Err(e) => return err(e),
        };
        let (networks, unsupported) = app::swappable_networks(&records, &deployed);
        json!({ "ok": true, "scope": scope, "networks": networks, "unsupported": unsupported }).to_string()
    }

    fn verdict(&self, chain_id: i64) -> String {
        if chain_id <= 0 {
            return err(format!("chain {chain_id} is not a valid chain id"));
        }
        let b = Budget::new(VERDICT_BUDGET);
        let Some(t) = b.take(VERDICT_BUDGET) else { return err("no time left to read the verdict") };
        Self::verdict_of(chain_id as u64, modules().eth_rpc_module.verified_proxy_status_with_timeout(chain_id, t)).to_string()
    }

    fn tokens(&self, chain_id: i64) -> String {
        self.arm();
        let b = Budget::new(TOKENS_BUDGET);
        self.ensure_eth_rpc(&b);
        self.ensure_token_list(&b);
        match self.offered(chain_id, &b).and_then(|rows| self.assets(chain_id, &rows, &b)) {
            Ok(v) => v.to_string(),
            Err(e) => err(e),
        }
    }

    fn catalogue(&self, chain_id: i64, query: String, offset: i64, limit: i64) -> String {
        let b = Budget::new(TOKENS_BUDGET);
        self.ensure_eth_rpc(&b);
        self.ensure_token_list(&b);
        let native = match self.assets(chain_id, &[], &b) {
            Ok(v) => rows_of(&v).into_iter().next().unwrap_or_default(),
            Err(e) => return err(e),
        };
        let matches = app::native_matches(&native, &query);
        let Some(t) = b.take(LOCAL_BUDGET) else { return err("no time left to read the token picker") };
        let page = modules().token_list_module.list_available_with_timeout(
            chain_id, &query, app::provider_offset(matches, offset), limit, t,
        );
        match reply(page, "token_list_module") {
            Ok(page) => app::merge_native_page(&native, matches, page, offset, limit).to_string(),
            Err(e) => err(e),
        }
    }

    fn balances(&self, chain_id: i64, address: String, tokens_json: String) -> String {
        let b = Budget::new(BALANCES_BUDGET);
        let extra = match app::parse_tokens(&tokens_json) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let tokens = match self.offered(chain_id, &b) {
            Ok(offered) => app::union_tokens(offered, extra),
            Err(e) => return err(e),
        };
        let Some(t) = b.take(ASSETS_BUDGET) else { return err("no time left to read the balances") };
        relay(
            modules().evm_assets_module.get_balances_with_timeout(chain_id, &address, &json!(tokens).to_string(), "alpha", t),
            "evm_assets_module",
        )
    }

    fn fee_tiers(&self, chain_id: i64) -> String {
        let b = Budget::new(FEES_BUDGET);
        let Some(t) = b.take(FEES_BUDGET) else { return err("no time left to price the fee") };
        relay(modules().fee_module.suggest_fees_with_timeout(chain_id, t), "fee_module")
    }

    fn accounts(&self) -> String {
        self.arm();
        let b = Budget::new(ACCOUNTS_BUDGET);
        let Some(t) = b.take(LOCAL_BUDGET) else { return err("no time left to read the accounts") };
        let accounts = match reply(modules().keystore_module.list_accounts_with_timeout(t), "keystore_module") {
            Ok(v) => v.get("accounts").cloned().unwrap_or_else(|| json!([])),
            Err(e) => return err(e),
        };
        let mut out = json!({ "ok": true, "accounts": accounts });
        // A name is a courtesy: an account list without one is still the answer.
        if let Some(t) = b.take(LOCAL_BUDGET) {
            if let Ok(v) = reply(modules().keystore_module.get_labels_with_timeout(t), "keystore_module") {
                out["labels"] = v.get("labels").cloned().unwrap_or_else(|| json!({}));
            }
        }
        if let Some(t) = b.take(LOCAL_BUDGET) {
            if let Ok(v) = reply(modules().keystore_module.get_account_wallets_with_timeout(t), "keystore_module") {
                out["wallets"] = v.get("wallets").cloned().unwrap_or_else(|| json!({}));
            }
        }
        out.to_string()
    }

    fn quote(&self, request_json: String) -> String {
        let b = Budget::new(QUOTE_BUDGET);
        let f = match app::parse_form(&request_json).and_then(|f| self.complete(f, &b)) {
            Ok(f) => f,
            Err(e) => return err(e),
        };
        let built = match self.build(&f, 0, &b) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let fee = match Self::sender_request(&built, &f, &caller_name(), "", &b) {
            Ok(Some((req, t))) => match reply(modules().tx_sender_module.prepare_with_timeout(&req.to_string(), t), "tx_sender_module") {
                Ok(v) => v,
                Err(e) => refusal(e),
            },
            Ok(None) => json!({ "ok": false, "error": "no time left to price the swap" }),
            Err(e) => refusal(e),
        };
        app::merged_quote(&built, &fee, &f).to_string()
    }

    fn swap(&self, request_json: String) -> String {
        let b = Budget::new(SWAP_BUDGET);
        let f = match app::parse_form(&request_json) {
            Ok(f) => f,
            Err(e) => return err(e),
        };
        if let Err(e) = self.in_scope(f.chain_id, &b) {
            return err(e);
        }
        let f = match self.complete(f, &b) {
            Ok(f) => f,
            Err(e) => return err(e),
        };
        // A FRESH build: the figures on screen may be a block old, and the minimum and the
        // deadline are set from this one.
        let deadline = now_secs() + f.deadline_mins as u64 * 60;
        let built = match self.build(&f, deadline, &b) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let via = caller_name();
        let purpose = app::swap_purpose(&f, &built, &via);
        let (req, t) = match Self::sender_request(&built, &f, &via, &purpose, &b) {
            Ok(Some(v)) => v,
            Ok(None) => return err("no time left to request approval"),
            Err(e) => return err(e),
        };
        // Deliberately no hash: nothing is signed or broadcast until a human approves.
        match reply(modules().tx_sender_module.send_with_timeout(&req.to_string(), t), "tx_sender_module") {
            Ok(mut v) => {
                v["chainId"] = json!(f.chain_id);
                v["from"] = json!(f.from);
                v["purpose"] = json!(purpose);
                v["amountOutMin"] = built.get("amountOutMin").cloned().unwrap_or(Value::Null);
                v["deadline"] = json!(deadline);
                v.to_string()
            }
            Err(e) => err(e),
        }
    }

    fn swap_status(&self, request_id: String) -> String {
        self.arm();
        let b = Budget::new(STATUS_BUDGET);
        let Some(t) = b.take(STATUS_BUDGET) else {
            return json!({ "ok": false, "final": false, "error": "no time left to read the swap" }).to_string();
        };
        match modules().tx_sender_module.send_status_with_timeout(&request_id, t) {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(mut v) if v.is_object() => {
                    v["final"] = json!(app::is_final(&v));
                    v.to_string()
                }
                Ok(_) => json!({ "ok": false, "final": false, "error": "tx_sender_module: the reply is not an object" }).to_string(),
                Err(e) => json!({ "ok": false, "final": false, "error": format!("tx_sender_module: {e}") }).to_string(),
            },
            // The sender may still be broadcasting behind a call that ran out of time.
            Err(e) => json!({ "ok": false, "final": false, "code": "unreachable",
                              "error": format!("tx_sender_module: {e:?}") }).to_string(),
        }
    }

    fn cancel_swap(&self, request_id: String) -> String {
        let b = Budget::new(CANCEL_BUDGET);
        let Some(t) = b.take(CANCEL_BUDGET) else { return err("no time left to cancel the swap") };
        relay(modules().tx_sender_module.cancel_send_with_timeout(&request_id, t), "tx_sender_module")
    }

    fn swaps(&self, address: String, chain_id: i64) -> String {
        self.arm();
        if chain_id <= 0 {
            return err(format!("chain {chain_id} is not a valid chain id"));
        }
        let b = Budget::new(HISTORY_BUDGET);
        let Some(t) = b.take(HISTORY_BUDGET) else { return err("no time left to read the swaps") };
        let history = match reply(modules().tx_sender_module.history_with_timeout(&address, chain_id, t), "tx_sender_module") {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let rows = history.get("transactions").and_then(Value::as_array).cloned().unwrap_or_default();
        json!({
            "ok": true, "chainId": chain_id, "address": address,
            "stillDue": history.get("stillDue").cloned().unwrap_or(json!(false)),
            "swaps": app::group_swaps(&rows, app::APP),
        })
        .to_string()
    }
}

// The registration hook. The generated glue declares it and the loader resolves it at dlopen;
// omitting it links cleanly and segfaults at set_context time on macOS.
#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<UniswapBackendImpl>();
}
