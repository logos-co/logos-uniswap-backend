//! uniswap_backend — the Uniswap app's backend, one composition of reusable EVM modules.
//!
//! Chains and verdicts come from `eth_rpc_module`, the token catalogue from `token_list_module`,
//! asset rows and balances from `evm_assets_module`, accounts from `keystore_module`, fee tiers
//! from `fee_module`, and quotes and calls from `uniswap_module`. Every swap leaves through
//! `tx_sender_module`, which reserves the nonces, asks a human once and broadcasts. No key
//! material reaches this module.
//!
//! Everything below the glue is plain Rust and is tested with `cargo test --no-default-features`.

pub mod app;
pub mod budget;
pub mod depinit;
pub mod units;
pub mod verified;

#[cfg(feature = "logos_module")]
mod glue;
