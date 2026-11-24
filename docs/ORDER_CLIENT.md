# hl-order: testnet signer and order client

Step 1 of the testnet order-path probe. Crate `crates/hl-order` signs three
Hyperliquid L1 actions and builds the `POST /exchange` body:

- `order`: one limit order, tif `Alo`, `Gtc` or `Ioc`, optional reduce-only and cloid
- `cancel`: by asset and exchange order id
- `scheduleCancel`: dead man's switch, with or without `time`

**Nothing is sent.** No transport ships in this crate. `ExchangeClient` takes a
`Transport` trait object; the tests use a fake one. No test, script or check makes
a network call to Hyperliquid.

## Testnet only

Ahmed approved testnet only. The crate cannot target mainnet:

- `Network` has one variant, `Testnet` (a `compile_fail` doctest proves
  `Network::Mainnet` does not exist).
- The only URL is `TESTNET_API_URL = https://api.hyperliquid-testnet.xyz`.
- The phantom-agent source is hard-wired to `"b"`; nothing can produce `"a"`.

Adding mainnet needs Ahmed's explicit approval and a separate PR.

## Signing scheme (verified against the SDK by golden vectors)

Source read: `hyperliquid-python-sdk==0.24.0` from PyPI (upstream
`hyperliquid-dex/hyperliquid-python-sdk`, master at
`2fdb18f9517675ea03695a0962bd19eece9c83f0` when read), files
`hyperliquid/utils/signing.py` (`action_hash`, `construct_phantom_agent`,
`l1_payload`, `sign_l1_action`, `sign_inner`, `float_to_wire`,
`order_request_to_order_wire`, `order_wires_to_order_action`) and
`hyperliquid/exchange.py` (`bulk_cancel`, `schedule_cancel`, `_post_action`).

1. `connectionId = keccak256(msgpack(action) || nonce:u64 BE || vault || expires)`
   - `vault` = `0x00`, or `0x01 || 20-byte address`
   - `expires` = empty, or `0x00 || expiresAfter:u64 BE` (the `0x00` is required)
2. Phantom agent `{source: "b", connectionId}` (`"a"` is mainnet, not built here).
3. EIP-712, primary type `Agent(string source,bytes32 connectionId)`, domain
   `{name: "Exchange", version: "1", chainId: 1337, verifyingContract: 0x0}`.
4. secp256k1 ECDSA, RFC 6979 deterministic nonce, low-s, `v = 27 + recid`.
   The body prints r and s as `eth_utils.to_hex(int)` does: `0x`, no leading
   zero nibbles (5 of the 12 goldens have a 63-nibble `s`, so this matters).

Wire shapes (msgpack key order is significant, it is part of the hash):

```text
order:          {type:"order", orders:[{a, b, p:"str", s:"str", r, t:{limit:{tif}}, c?}], grouping:"na"}
cancel:         {type:"cancel", cancels:[{a, o}]}
scheduleCancel: {type:"scheduleCancel", time?}
body:           {action, nonce, signature:{r,s,v}, vaultAddress, expiresAfter}
```

Prices and sizes are `Decimal8` (integer count of 1e-8), never floats. `to_wire`
equals the SDK's `float_to_wire` (8 decimals max, trailing zeros stripped).

## Price and size rules (perps)

- Price: at most 5 significant figures (integers always allowed) and at most
  `6 - szDecimals` decimals. `round_px_perp` rounds the exact decimal to 5
  significant figures, then to `6 - szDecimals` decimals, half-to-even, matching
  the SDK examples' `round(float(f"{px:.5g}"), 6 - szDecimals)` on 400 random
  non-tie inputs. On exact decimal ties the float version depends on binary
  representation; we are deterministic there.
- Size: a multiple of `10^-szDecimals`. `floor_to_lot` rounds down, so the probe
  should size for about $12 notional to stay above the $10 minimum after flooring.
- Spot rules (`8 - szDecimals` decimals) are not implemented.

## Golden vectors

`scripts/gen_hl_order_golden.py` imports the SDK's own helpers, signs with the
throwaway public key `0x0123456789012345678901234567890123456789012345678901234567890123`
(address `0x14791697260E4c9A71f18484C9f997B308e59325`), `is_mainnet=False`, fixed
nonces, and writes `golden/hl-order/vectors.json`. Rust must reproduce, for each
of 12 cases, the action msgpack bytes, the connectionId, r, s, v and the exact
JSON body. Cases: Alo buy, Gtc sell, Ioc reduce-only, cloid, vault, expiresAfter,
vault + expiresAfter, spot-style asset 10000, cancel, cancel with a 2^40 oid,
scheduleCancel with and without time. Plus 312 `float_to_wire` and 400
price-rounding vectors.

```bash
pip install --target "$HOME/pysdk" hyperliquid-python-sdk==0.24.0
PYTHONPATH="$HOME/pysdk" python3 scripts/gen_hl_order_golden.py > golden/hl-order/vectors.json
```

`scripts/check.sh` runs the golden test and, when the SDK is installed, regenerates
the vectors and fails on any drift.

## Crates

- `secp256k1` (bindings to libsecp256k1, Bitcoin Core's C library) with
  `recovery`: recoverable RFC 6979 signatures, low-s. One signing context per
  process, built with the first `Signer` and reused; a signature creates no
  context and does not allocate (`tests/alloc.rs`). Building it needs a C
  compiler for the target (`secp256k1-sys` compiles vendored C).
- `k256` (RustCrypto, pure Rust) is a dev-dependency only: the backend until
  PR #20, kept as the oracle that every signature is compared with
  (`src/ecdsa.rs`, `tests/backend_equiv.rs`).
- `sha3` `Keccak256`: implements `io::Write`, so the action is msgpack-encoded
  straight into the hasher; signing does not allocate.
- `rmp` (low-level msgpack writer): minimal-width encodings identical to Python's
  `msgpack.packb`, field order under our control.

## Timing hooks

`ExchangeClient::send` returns `RttStamps { before_write, after_read }`
(`Instant`s taken right before the transport is handed the body and right after
it returns the full response) for the later RTT probe.

## Numbers

`cargo bench -p hl-order` on the Reason cloud VM (2 vCPU Intel Xeon, shared, noisy):
`sign_order` about 47 us (65.9 us without `precomputed-tables`), `request_body`
about 0.25 us. Signing dominates; the libsecp256k1 C library is usually faster
and is a candidate if Latency wants it.

## Unverified on the wire

Everything above is checked against the SDK offline only. Not verified until
Ahmed approves the probe and an order is actually sent:

- that testnet accepts source `"b"` with chainId 1337 as the SDK assumes;
- testnet's response shapes, error strings, rate limits and nonce window;
- asset indices and `szDecimals` on testnet (must be read from testnet `meta`);
- real round-trip times.
