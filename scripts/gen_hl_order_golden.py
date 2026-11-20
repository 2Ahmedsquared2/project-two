#!/usr/bin/env python3
"""Generate golden vectors for crates/hl-order from the official Hyperliquid Python SDK.

Pinned SDK: hyperliquid-python-sdk==0.24.0 (PyPI), file hyperliquid/utils/signing.py
(action_hash, construct_phantom_agent, l1_payload, sign_l1_action, float_to_wire,
order_request_to_order_wire, order_wires_to_order_action) and hyperliquid/exchange.py
(cancel / schedule_cancel action shapes, _post_action payload).

Usage:
    pip install --target "$HOME/pysdk" hyperliquid-python-sdk==0.24.0
    PYTHONPATH="$HOME/pysdk" python3 scripts/gen_hl_order_golden.py > golden/hl-order/vectors.json

Safety: this script makes NO network calls. The key below is a throwaway, publicly
known test key (it appears in many public examples); it must never hold funds.
Every vector is signed with is_mainnet=False (testnet, phantom-agent source "b").
"""

import json
import random
from decimal import Decimal, ROUND_HALF_EVEN

import msgpack
from eth_account import Account
from importlib.metadata import version

from hyperliquid.utils.signing import (
    action_hash,
    float_to_wire,
    order_request_to_order_wire,
    order_wires_to_order_action,
    sign_l1_action,
)
from hyperliquid.utils.types import Cloid

TEST_KEY = "0x0123456789012345678901234567890123456789012345678901234567890123"
IS_MAINNET = False
VAULT = "0x1111111111111111111111111111111111111111"

wallet = Account.from_key(TEST_KEY)


def order_action(asset, is_buy, px, sz, reduce_only, tif, cloid=None):
    req = {
        "coin": "unused",
        "is_buy": is_buy,
        "sz": float(sz),
        "limit_px": float(px),
        "order_type": {"limit": {"tif": tif}},
        "reduce_only": reduce_only,
    }
    if cloid is not None:
        req["cloid"] = Cloid.from_str(cloid)
    return order_wires_to_order_action([order_request_to_order_wire(req, asset)])


def vector(name, inputs, action, nonce, vault=None, expires_after=None):
    sig = sign_l1_action(wallet, action, vault, nonce, expires_after, IS_MAINNET)
    # Same payload exchange.py::_post_action builds, compact JSON.
    body = {
        "action": action,
        "nonce": nonce,
        "signature": sig,
        "vaultAddress": vault,
        "expiresAfter": expires_after,
    }
    return {
        "name": name,
        "inputs": inputs,
        "nonce": nonce,
        "vault": vault,
        "expires_after": expires_after,
        "action_msgpack_hex": msgpack.packb(action).hex(),
        "connection_id_hex": action_hash(action, vault, nonce, expires_after).hex(),
        "r": sig["r"],
        "s": sig["s"],
        "v": sig["v"],
        "body_json": json.dumps(body, separators=(",", ":")),
    }


def order_inputs(asset, is_buy, px, sz, reduce_only, tif, cloid=None):
    return {
        "kind": "order",
        "asset": asset,
        "is_buy": is_buy,
        "px": px,
        "sz": sz,
        "reduce_only": reduce_only,
        "tif": tif,
        "cloid": cloid,
    }


def signing_vectors():
    out = []
    n0 = 1_700_000_000_000

    def order(name, nonce, asset, is_buy, px, sz, ro, tif, cloid=None, vault=None, exp=None):
        out.append(
            vector(
                name,
                order_inputs(asset, is_buy, px, sz, ro, tif, cloid),
                order_action(asset, is_buy, px, sz, ro, tif, cloid),
                nonce,
                vault,
                exp,
            )
        )

    order("alo_buy", n0 + 1, 0, True, "30000", "0.001", False, "Alo")
    order("gtc_sell", n0 + 2, 1, False, "2501.5", "0.0123", False, "Gtc")
    order("ioc_buy_reduce_only", n0 + 3, 5, True, "0.012345", "1000", True, "Ioc")
    order("gtc_buy_cloid", n0 + 4, 0, True, "29999", "0.0004", False, "Gtc",
          cloid="0x00000000000000000000000000000001")
    order("alo_buy_vault", n0 + 5, 0, True, "30000", "0.001", False, "Alo", vault=VAULT)
    order("alo_buy_expires_after", n0 + 6, 0, True, "30000", "0.001", False, "Alo",
          exp=n0 + 60_000)
    order("gtc_sell_vault_and_expires", n0 + 7, 1, False, "2501.5", "0.0123", False, "Gtc",
          vault=VAULT, exp=n0 + 60_000)
    order("large_asset_spot_index", n0 + 8, 10_000, True, "0.5", "20", False, "Gtc")

    out.append(vector("cancel", {"kind": "cancel", "asset": 0, "oid": 123_456_789},
                      {"type": "cancel", "cancels": [{"a": 0, "o": 123_456_789}]}, n0 + 10))
    out.append(vector("cancel_big_oid", {"kind": "cancel", "asset": 7, "oid": 2**40 + 3},
                      {"type": "cancel", "cancels": [{"a": 7, "o": 2**40 + 3}]}, n0 + 11))
    out.append(vector("schedule_cancel_with_time", {"kind": "scheduleCancel", "time": n0 + 30_000},
                      {"type": "scheduleCancel", "time": n0 + 30_000}, n0 + 12))
    out.append(vector("schedule_cancel_without_time", {"kind": "scheduleCancel", "time": None},
                      {"type": "scheduleCancel"}, n0 + 13))
    return out


def float_to_wire_vectors(rng):
    out = []
    fixed = ["0", "1", "10", "0.1", "0.00000001", "123.456", "30000", "2501.5", "0.012345",
             "99999.99999999", "1e-8", "1234567.89"]
    for s in fixed:
        out.append({"input": format(Decimal(s), "f"), "wire": float_to_wire(float(s))})
    for _ in range(300):
        units = rng.randrange(0, 10**15)  # value = units * 1e-8, up to 1e7
        d = Decimal(units).scaleb(-8)
        out.append({"input": format(d, "f"), "wire": float_to_wire(float(d))})
    return out


def is_tie(d, quantum):
    # True when d sits exactly halfway between two multiples of quantum.
    q = d / quantum
    return (q - q.to_integral_value(rounding="ROUND_FLOOR")) == Decimal("0.5")


def round_px_vectors(rng):
    """Mirror of the SDK examples' rounding: round(float(f"{px:.5g}"), 6 - szDecimals).

    Inputs that are an exact decimal tie at either rounding step are skipped: the float
    result there depends on binary representation, while the Rust code rounds the exact
    decimal half-to-even. The skipped count is recorded.
    """
    out, skipped = [], 0
    while len(out) < 400:
        szd = rng.randrange(0, 7)
        units = rng.randrange(1, 10**14)
        d = Decimal(units).scaleb(-8)
        exp = d.adjusted()  # power of ten of the leading digit
        q1 = Decimal(1).scaleb(exp - 4)
        if is_tie(d, q1):
            skipped += 1
            continue
        step1 = d.quantize(q1, rounding=ROUND_HALF_EVEN)
        q2 = Decimal(1).scaleb(-(6 - szd))
        if is_tie(step1, q2):
            skipped += 1
            continue
        py = round(float(f"{float(d):.5g}"), 6 - szd)
        out.append({"input": format(d, "f"), "sz_decimals": szd, "wire": float_to_wire(py)})
    return out, skipped


def main():
    rng = random.Random(20260101)
    px, skipped = round_px_vectors(rng)
    doc = {
        "generator": "scripts/gen_hl_order_golden.py",
        "sdk": "hyperliquid-python-sdk==" + version("hyperliquid-python-sdk"),
        "msgpack": msgpack.version,
        "test_key": TEST_KEY,
        "signer_address": wallet.address,
        "is_mainnet": IS_MAINNET,
        "signing": signing_vectors(),
        "float_to_wire": float_to_wire_vectors(rng),
        "round_px_perp": px,
        "round_px_perp_skipped_ties": skipped,
    }
    print(json.dumps(doc, indent=1))


if __name__ == "__main__":
    main()
