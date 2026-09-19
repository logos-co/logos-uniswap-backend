//! A source-shape guard on `glue.rs`, which `--no-default-features` cannot compile: every
//! outbound call is bounded, the keystore client only reads, money leaves through one sender
//! call, the words a human approves are composed in `app.rs` alone, and eth_rpc's defaults are
//! asked for with no gate. Every check ships with the mutant it must reject.

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

fn eth_rpc_calls(src: &str) -> Vec<String> {
    calls(src).into_iter().filter(|(c, _)| c == "eth_rpc_module").map(|(_, m)| m).collect()
}

/// Sites that ensure token_list's defaults without first asking eth_rpc for its own.
fn unpaired_default_sites(src: &str) -> usize {
    let flat: String = code_only(src).chars().filter(|c| !c.is_whitespace()).collect();
    flat.matches("self.ensure_token_list(").count()
        - flat.matches("self.ensure_eth_rpc(&b);self.ensure_token_list(&b);").count()
}

/// The body of `fn <name>`, braces matched on the blanked code.
fn body(src: &str, name: &str) -> String {
    let code = code_only(src);
    let at = code.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("no fn {name}"));
    let open = at + code[at..].find('{').expect("a body");
    let mut depth = 0;
    for (k, ch) in code[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' if depth == 1 => return code[open..=open + k].to_string(),
            '}' => depth -= 1,
            _ => {}
        }
    }
    code[open..].to_string()
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

/// eth_rpc owns its defaults and this backend asks for them wherever it ensures token_list's,
/// and in front of the registry read. A `config_status` gate would strand them: a store another
/// app already wrote to reads `configured` and still lacks what it needs.
#[test]
fn eth_rpc_defaults_are_asked_for_without_a_gate() {
    let asked = eth_rpc_calls(GLUE);
    assert_eq!(asked.iter().filter(|m| *m == "init_defaults_with_timeout").count(), 1, "{asked:?}");
    assert!(!asked.iter().any(|m| m.starts_with("config_status")), "{asked:?}");
    assert_eq!(unpaired_default_sites(GLUE), 0);
    assert!(body(GLUE, "chain_configs").contains("self.ensure_eth_rpc(b)"));

    let gated = GLUE.replacen(
        "let applied = modules().eth_rpc_module.init_defaults_with_timeout(t);",
        "let _ = modules().eth_rpc_module.config_status_with_timeout(t);\n        let applied = modules().eth_rpc_module.init_defaults_with_timeout(t);",
        1,
    );
    assert_ne!(gated, GLUE, "the mutant applies");
    assert!(eth_rpc_calls(&gated).iter().any(|m| m.starts_with("config_status")));
    let unpaired = GLUE.replacen("self.ensure_eth_rpc(&b);\n        self.ensure_token_list(&b);", "self.ensure_token_list(&b);", 1);
    assert_ne!(unpaired, GLUE, "the mutant applies");
    assert_eq!(unpaired_default_sites(&unpaired), 1);
    let skipped = GLUE.replacen("        self.ensure_eth_rpc(b);\n", "", 1);
    assert_ne!(skipped, GLUE, "the mutant applies");
    assert!(!body(&skipped, "chain_configs").contains("self.ensure_eth_rpc(b)"));
}
