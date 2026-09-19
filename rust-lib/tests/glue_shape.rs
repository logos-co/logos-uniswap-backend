//! A source-shape guard on `glue.rs`, which `--no-default-features` cannot compile: every
//! outbound call is bounded, the keystore client only reads, money leaves through one sender
//! call, and the words a human approves are composed in `app.rs` alone. Every check ships with
//! the mutant it must reject.

const GLUE: &str = include_str!("../src/glue.rs");

/// The file with comments and string literals blanked out, offsets preserved.
fn code_only(src: &str) -> String {
    let (b, mut out) = (src.as_bytes(), src.as_bytes().to_vec());
    let (mut i, mut in_str, mut in_comment) = (0usize, false, false);
    while i < b.len() {
        match (in_str, in_comment, b[i]) {
            (false, false, b'"') => in_str = true,
            (false, false, b'/') if b.get(i + 1) == Some(&b'/') => in_comment = true,
            (true, _, b'\\') => {
                out[i] = b' ';
                out[i + 1] = b' ';
                i += 2;
                continue;
            }
            (true, _, b'"') => in_str = false,
            (_, true, b'\n') => in_comment = false,
            _ => {}
        }
        if (in_str && b[i] != b'"') || in_comment {
            out[i] = b' ';
        }
        i += 1;
    }
    String::from_utf8(out).expect("blanking replaces bytes one for one")
}

/// Every `modules().<client>.<method>(` call site, as (client, method).
fn calls(src: &str) -> Vec<(String, String)> {
    let code = code_only(src);
    let mut out = Vec::new();
    for (at, _) in code.match_indices("modules().") {
        let rest = &code[at + "modules().".len()..];
        let client: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let after = rest[client.len()..].trim_start();
        let Some(after) = after.strip_prefix('.') else { continue };
        let method: String = after.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        out.push((client, method));
    }
    out
}

fn unbounded(src: &str) -> Vec<String> {
    calls(src)
        .into_iter()
        .filter(|(_, m)| !m.ends_with("_with_timeout") && !m.starts_with("on_"))
        .map(|(c, m)| format!("{c}.{m}"))
        .collect()
}

fn keystore_writes(src: &str) -> Vec<String> {
    const READS: [&str; 4] =
        ["list_accounts_with_timeout", "get_labels_with_timeout", "get_account_wallets_with_timeout", "on_accounts_changed"];
    calls(src)
        .into_iter()
        .filter(|(c, m)| c == "keystore_module" && !READS.contains(&m.as_str()))
        .map(|(_, m)| m)
        .collect()
}

fn send_sites(src: &str) -> usize {
    calls(src).iter().filter(|(c, m)| c == "tx_sender_module" && m == "send_with_timeout").count()
}

fn uniswap_calls(src: &str) -> Vec<String> {
    calls(src)
        .into_iter()
        .filter(|(c, m)| c == "uniswap_module" && m != "get_chains_with_timeout" && m != "build_swap_with_timeout")
        .map(|(_, m)| m)
        .collect()
}

/// Wording a human approves, composed here instead of in `app.rs`.
fn purpose_outside_app(src: &str) -> bool {
    src.contains("on Uniswap") || src.contains("for at least")
}

#[test]
fn every_outbound_call_is_bounded() {
    assert!(calls(GLUE).len() >= 20, "the scan sees the calls: {:?}", calls(GLUE));
    assert_eq!(unbounded(GLUE), Vec::<String>::new());
    let mutant = GLUE.replacen("fee_module.suggest_fees_with_timeout(chain_id, t)", "fee_module.suggest_fees(chain_id)", 1);
    assert_ne!(mutant, GLUE, "the mutant applies");
    assert_eq!(unbounded(&mutant), vec!["fee_module.suggest_fees".to_string()]);
}

#[test]
fn the_keystore_client_only_reads() {
    assert_eq!(keystore_writes(GLUE), Vec::<String>::new());
    let mutant = GLUE.replacen("keystore_module.get_labels_with_timeout(t)", "keystore_module.approve_with_timeout(t)", 1);
    assert_ne!(mutant, GLUE, "the mutant applies");
    assert_eq!(keystore_writes(&mutant), vec!["approve_with_timeout".to_string()]);
}

#[test]
fn money_leaves_through_one_sender_call() {
    assert_eq!(send_sites(GLUE), 1);
    let mutant = GLUE.replacen("tx_sender_module.prepare_with_timeout(", "tx_sender_module.send_with_timeout(", 1);
    assert_ne!(mutant, GLUE, "the mutant applies");
    assert_eq!(send_sites(&mutant), 2);
}

#[test]
fn uniswap_module_is_only_asked_for_deployments_and_built_swaps() {
    assert_eq!(uniswap_calls(GLUE), Vec::<String>::new());
    let mutant = GLUE.replacen("uniswap_module.get_chains_with_timeout(t)", "uniswap_module.configure_with_timeout(t)", 1);
    assert_ne!(mutant, GLUE, "the mutant applies");
    assert_eq!(uniswap_calls(&mutant), vec!["configure_with_timeout".to_string()]);
}

#[test]
fn the_words_a_human_approves_are_composed_in_app_alone() {
    assert!(!purpose_outside_app(GLUE));
    assert!(GLUE.contains("app::swap_purpose("));
    let mutant = GLUE.replacen("let purpose = app::swap_purpose(&f, &built, &via);",
                               "let purpose = format!(\"Swap on Uniswap, via {via}\");", 1);
    assert_ne!(mutant, GLUE, "the mutant applies");
    assert!(purpose_outside_app(&mutant));
}

#[test]
fn no_lock_is_held_because_none_exists() {
    let code = code_only(GLUE);
    assert!(!code.contains("Mutex") && !code.contains("RwLock"), "glue keeps no locked state");
}
