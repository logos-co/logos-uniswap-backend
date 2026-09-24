# Uniswap backend

`uniswap_backend` is the Uniswap app's backend and its headless surface. `uniswap_ui` renders
what it answers and calls nothing else; anything the view does is a call anyone can make with
`logosctl`.

It composes reusable EVM modules and owns only the Uniswap app's rules:

| Concern | Owning module |
| --- | --- |
| Chain registry, scope and verified-proxy verdicts | `eth_rpc_module` |
| Token catalogue and the offered set | `token_list_module` |
| Asset rows, balances and exact units | `evm_assets_module` |
| Accounts and their names (read-only) | `keystore_module` |
| Fee tiers | `fee_module` |
| Uniswap deployments, quotes and the calls that make a swap | `uniswap_module` |
| Nonces, the one approval, broadcast and history | `tx_sender_module` |
| What a swap request means, what the human is told, how history groups into swaps | this module |

It holds no key material and signs nothing: every swap leaves through `tx_sender_module`, and
the human's yes is taken by `evm_signer_ui` (or `evm_signer_cli`), once, for every call.

On start, wherever it ensures `token_list_module`'s defaults, and in front of the registry read,
it asks `eth_rpc_module.init_defaults` for the default chains until one call lands. It does not
ask `config_status` first: eth_rpc fills only what is absent and seeds a default chain at most
once per device. `token_list_module.init_defaults` is asked the same way, on start and in front
of the token reads: token_list writes its defaults only when nothing is configured.

## Contract

Reads:

- `networks()` — eth_rpc's enabled, in-scope chains that `uniswap_module` holds a deployment
  for, plus `unsupported`: the enabled chains Uniswap is not on.
- `verdict(chain_id)` — eth_rpc's verified-proxy verdict; a verdict that cannot be read is a
  blocking `mode: "unknown"` one, never `off`.
- `tokens(chain_id)`, `catalogue(chain_id, query, offset, limit)` — the native coin and the
  offered tokens; the paged picker with the native row first.
- `balances(chain_id, address, tokens_json)` — the native coin, the offered tokens, and any
  token rows the caller adds.
- `fee_tiers(chain_id)`, `accounts()` — relayed; `accounts` joins the keystore's accounts,
  names and wallets.

Swaps:

- `quote(request)` — prices a swap without side effects. The request is the swap form:
  `{ chainId, from, tokenIn, tokenOut, amountUnits | amountIn, decimalsIn?, decimalsOut?,
  symbolIn?, symbolOut?, slippageBps?, deadlineMins?, tier?, recipient?, maxFeePerGas?,
  maxPriorityFeePerGas?, gasLimits?, nonce? }`. A token is an address, `ETH`, or an offered
  symbol; missing decimals are looked up and never assumed. The fee fields are the user's own,
  in wei: they reach the sender as given, `gasLimits` one per call in build order (`null` leaves
  a call estimated). The reply is `uniswap_module`'s build with display figures and the
  sender's pricing under `fee`.
- `swap(request)` — builds the swap afresh, tags every call, writes the purpose and asks the
  sender: `{ ok, pending, requestId, handle, purpose, amountOutMin, deadline }`.
- `swap_status(request_id)` — the sender's `send_status` and its `final`. Polling is the
  broadcast; `final: false` means ask again, refusals included — the sender calls a refusal
  final only when it holds no such request. A sender that predates `final` is read off
  `status`: only `awaitingApproval` and `broadcasting` still move, and no refusal is final.
- `cancel_swap(request_id)`, `swaps(address, chain_id)` — the app's swaps, newest first, one
  entry per bundle with its legs. A bundle's `status` is its worst leg's: `failed`, `blocked`,
  `stalled`, `pending`, then `replaced` when the swap leg itself was replaced (a replaced
  approval is not the swap), else `confirmed`. A confirmed swap carries `received`
  (`+Display`/`Exact`): what its receipt says reached the recipient — the ERC-20 Transfers of
  the token bought, or for ether EIP-7708's ether logs. Absent when no such log was decoded.

Events: `networks_changed`, `tokens_changed`, `accounts_changed`, `swap_status_changed`,
`swaps_changed`, relayed from the modules that own the facts.

## Who asked

`tx_sender_module` records the module that called it, so the sender's `origin` is always
`uniswap_backend`. The backend reads its own caller the same way and passes it on: every call
carries `meta.via`, and the purpose the human approves names it — "Swap 1000 TKN for at least
0.0994005994005994 ETH on Uniswap, via uniswap_ui [asked by uniswap_backend]", every digit. A
swap made with `logosctl` reads `via host`. Every call is also tagged `meta.app = "uniswap_ui"`,
which is how the app finds its swaps in a history it shares with the wallet.

## Headless

No CLI twin is needed: nothing here is gated on the caller, and `evm_signer_cli` holds the
keystore's approver role.

```bash
logosctl watch evm_signer_cli --event prompt          # load the approver before the swap
logosctl call uniswap_backend quote @swap.json         # {"chainId":1,"from":"0x…","tokenIn":"USDC","tokenOut":"ETH","amountUnits":"100"}
logosctl call uniswap_backend swap @swap.json          # → requestId, handle
logosctl call evm_signer_cli approve <handle> <bundle_id> @pw.txt
logosctl call uniswap_backend swap_status <requestId>  # repeat until "final":true
```

`doctests/uniswap-anvil-swap.test.yaml` runs exactly this against a local Anvil chain.

## Budgets

Each entry point spends one allowance across all its calls. A quote gets 58 s and a swap
60 s: the build (40 s; uniswap_module reads for 30 s and probes the winner for 8 s) and then
the sender, which is told what is left. The build is long for the verified proxy's sake: it
fetches a proof for every slot a quote touches, and a mainnet quote batch measured 18–29 s.
The view waits 62 s for those two and 20 s for everything else.

## Build and test

```bash
cargo test --manifest-path rust-lib/Cargo.toml --no-default-features --locked
nix build .#default .#lgx
nix run github:logos-co/logos-doctest -- run doctests/uniswap-anvil-swap.test.yaml
```

`rust-lib/src/app.rs` holds every rule and is tested there; `rust-lib/tests/glue_shape.rs`
reads `glue.rs` as text and pins that every call is bounded, the keystore client only reads,
money leaves through one sender call, and the purpose is composed in `app.rs` alone.
