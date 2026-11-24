#!/usr/bin/env python3
"""Convert the book section of an `hl-node translate-abci-state` JSON dump
into the L4 seed that `examples/replay.rs` loads (l4Snapshots shape):

    [height, [[coin, [[[user, order], ...bids], [[user, order], ...asks]]], ...],
     {"next_oid": N, "source": ..., "books": [...]}]

Input is the pretty-printed fragment AWS cuts from the translation
(`perp_dexs[].books[]`, optionally followed by `spot_books[]`). The layout
below is read from the data, not from documentation:

  book.halfs[0|1].levels = [[price_key, level], ...]   (bid keys are -px)
  level.f / level.l       = head / tail index into book.bod.e
  bod.e[i] = {"o": order} (live) or {"v": next_free} (free slot)
  order.n / order.p       = next / prev arena index (absent = none)
  order.o = oid, order.u = user, order.r = remaining sz (integer units),
  order.c = {t: time ms, s: "B"|"A", l: limit px (integer units), c: cloid,
             r: reduce-only, ...}
  level.s = {s: total sz, n: order count}   (checked against the walk)

Every level walk is checked: it must end at `l`, visit exactly `s.n` live
orders whose remaining sizes sum to `s.s`, all on the half's side and at the
level's price. Any failure aborts (no silent repair).

Usage: abci_books_to_l4.py BOOKS_JSON META_DIR HEIGHT NEXT_OID OUT_JSON
"""

import json
import sys


def load_names(meta_dir):
    """Map native internal asset id -> coin name, plus decimals."""
    out = {}
    meta = json.load(open(f"{meta_dir}/meta.json"))
    for i, u in enumerate(meta["universe"]):
        out[i] = (u["name"], u["szDecimals"], 6 - u["szDecimals"])
    spot = json.load(open(f"{meta_dir}/spotMeta.json"))
    toks = {t["index"]: t["szDecimals"] for t in spot["tokens"]}
    spot_names = {}
    for u in spot["universe"]:
        d = toks[u["tokens"][0]]
        spot_names[u["index"]] = (u["name"], d, 8 - d)
    dexs = json.load(open(f"{meta_dir}/perpDexs.json"))
    for k, d in enumerate(dexs):
        if not d:
            continue
        m = json.load(open(f"{meta_dir}/meta_{d['name']}.json"))
        for i, u in enumerate(m["universe"]):
            # Native internal id for builder dex k is k*10000+i (observed:
            # the 2nd dex's books carry assets 10000..); the API id adds 100000.
            out[k * 10000 + i] = (u["name"], u["szDecimals"], 6 - u["szDecimals"])
    return out, spot_names


def fmt(v, dec):
    s = str(v).rjust(dec + 1, "0")
    if dec == 0:
        return s
    ip, fp = s[:-dec], s[-dec:].rstrip("0")
    return ip if not fp else f"{ip}.{fp}"


def walk_book(book, names, stats):
    asset = book["asset"]
    if asset not in names:
        raise SystemExit(f"asset {asset} not in meta")
    coin, szd, pxd = names[asset]
    arena = book["bod"]["e"]
    sides = []
    for hi, half in enumerate(book["halfs"]):
        want = "B" if hi == 0 else "A"
        out = []
        prev_px = None
        for key, lv in half["levels"]:
            px = -key if hi == 0 else key
            if prev_px is not None and not (px < prev_px if hi == 0 else px > prev_px):
                raise SystemExit(f"{coin} half {hi}: levels not best-first at {px}")
            prev_px = px
            i, n, tot = lv["f"], 0, 0
            while True:
                ent = arena[i]
                if "o" not in ent:
                    raise SystemExit(f"{coin} px {px}: arena {i} is a free slot")
                o = ent["o"]
                c = o["c"]
                if c["s"] != want or c["l"] != px:
                    raise SystemExit(f"{coin} px {px}: arena {i} side/px {c['s']}/{c['l']}")
                order = {
                    "coin": coin,
                    "side": want,
                    "limitPx": fmt(px, pxd),
                    "sz": fmt(o["r"], szd),
                    "oid": o["o"],
                    "timestamp": c["t"],
                    "origSz": fmt(c["S"], szd),
                    "cloid": c.get("c"),
                    "reduceOnly": bool(c.get("r", False)),
                    "tif": o.get("t"),
                }
                out.append([o["u"], order])
                n += 1
                tot += o["r"]
                stats["reduce_only"] += order["reduceOnly"]
                if i == lv["l"]:
                    break
                i = o.get("n", 0)
                if n > lv["s"]["n"]:
                    raise SystemExit(f"{coin} px {px}: walk longer than s.n")
            if n != lv["s"]["n"] or tot != lv["s"]["s"]:
                raise SystemExit(
                    f"{coin} px {px}: walk n={n} sz={tot} vs s={lv['s']}"
                )
            stats["levels"] += 1
        stats["orders"] += len(out)
        sides.append(out)
    live = sum(1 for e in arena if "o" in e)
    if live != len(sides[0]) + len(sides[1]):
        raise SystemExit(f"{coin}: {live} live arena orders, walked {len(sides[0]) + len(sides[1])}")
    oids = {o["oid"] for s in sides for _, o in s}
    if {k for k, _ in book["oid_to_key"]} != oids:
        raise SystemExit(f"{coin}: oid_to_key differs from walked oids")
    return [coin, sides]


def iter_books(path):
    """Yield (section, book_json) from the pretty-printed fragment."""
    section = None
    buf = None
    with open(path) as f:
        for line in f:
            if line.startswith('    "perp_dexs": ['):
                section = "perp"
                continue
            if line.startswith('    "spot_books": ['):
                section = "spot"
                continue
            if section == "perp":
                if line.startswith('        "books": ['):
                    buf = []
                    continue
                if buf is not None:
                    if line.startswith("        ]"):
                        buf = None
                        continue
                    buf.append(line)
                    if line.startswith("          }"):
                        yield "perp", json.loads("".join(buf).rstrip().rstrip(","))
                        buf = []
            elif section == "spot":
                # spot_books is an array of books at indent 6.
                if line.startswith("      {"):
                    buf = [line]
                    continue
                if buf is not None:
                    buf.append(line)
                    if line.startswith("      }"):
                        yield "spot", json.loads("".join(buf).rstrip().rstrip(","))
                        buf = None


def main():
    src, meta_dir, height, next_oid, dst = sys.argv[1:6]
    names, spot_names = load_names(meta_dir)
    spot_by_internal = {10000 + k: v for k, v in spot_names.items()}
    stats = {"books": 0, "levels": 0, "orders": 0, "reduce_only": 0, "spot_books": 0}
    books = []
    seen = set()
    for section, book in iter_books(src):
        table = names if section == "perp" else spot_by_internal
        cb = walk_book(book, table, stats)
        if cb[0] in seen:
            raise SystemExit(f"duplicate book {cb[0]}")
        seen.add(cb[0])
        books.append(cb)
        stats["books"] += 1
        stats["spot_books"] += section == "spot"
    max_oid = max((o["oid"] for _, s in books for side in s for _, o in side), default=0)
    if int(next_oid) <= max_oid:
        raise SystemExit(f"next_oid {next_oid} <= max resting oid {max_oid}")
    info = {
        "next_oid": int(next_oid),
        "source": "hl-node translate-abci-state book section (OUR NODE)",
        "books": sorted(seen),
        "stats": stats,
    }
    with open(dst, "w") as f:
        json.dump([int(height), books, info], f, separators=(",", ":"))
    print(json.dumps(stats), "max_oid", max_oid, file=sys.stderr)


if __name__ == "__main__":
    main()
