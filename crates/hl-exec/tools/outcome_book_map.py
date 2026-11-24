#!/usr/bin/env python3
"""Map outcome-market coins ("#E") to the native spot book that holds them.

Outcome books are not named in spotMeta: in our node's state they sit in
`spot_books[]` among the indices spotMeta does not list. Nothing in the book
itself carries E, so the mapping is read from data:

  1. oid match: every "#E" order status in the window whose oid rests in the
     seed's spot cut names that book (a coin must hit exactly one book, and a
     book exactly one coin; anything else aborts);
  2. pair: E = 10*outcome + side, and the two sides of an outcome sit on
     consecutive book indices (side 0 at k, side 1 at k+1). Checked on every
     pair where both sides are oid-matched; a coin whose other side is matched
     but which has no oid hit of its own gets that neighbour, only if the
     neighbour book is spotMeta-unlisted and not already taken.

  3. state (optional STATE_MAP_JSON): {"#E": spot_book_index} read from the
     native state's own chain (outcome E -> side token T = ots[E].s[side].t
     -> token_infos[T].spots[0] = spot pair J = spot_books[J].asset). Every
     coin it shares with rules 1-2 must agree (else abort); a coin they left
     out gets its book from here if that book is unlisted and not taken.

Coins with none of these are left out (the replay counts them as
unseeded-book).

Usage: outcome_book_map.py SPOT_CUT_JSON[.gz] META_DIR ORDER_STATUSES_JSONL OUT_JSON [STATE_MAP_JSON]
"""

import collections
import json
import re
import sys

from abci_books_to_l4 import iter_books, load_names

STATUS = re.compile(r'"coin":"(#\d+)","side":"[AB]","limitPx":"[^"]+","sz":"[^"]+","oid":(\d+)')


def main():
    cut, meta_dir, statuses, dst = sys.argv[1:5]
    state = {}
    if len(sys.argv) > 5:
        state = {c: int(a) for c, a in json.load(open(sys.argv[5])).items()}
    _, spot_names = load_names(meta_dir)
    oid_book = {}
    unlisted = set()
    for _, book in iter_books(cut):
        a = book["asset"]
        if a not in spot_names:
            unlisted.add(a)
        for e in book["bod"]["e"]:
            if "o" in e:
                oid_book[e["o"]["o"]] = a
    hits = collections.defaultdict(collections.Counter)
    coins = set()
    with open(statuses) as f:
        for line in f:
            if '"#' not in line:
                continue
            for coin, oid in STATUS.findall(line):
                coins.add(coin)
                a = oid_book.get(int(oid))
                if a is not None:
                    hits[coin][a] += 1
    out = {}
    for coin, c in hits.items():
        if len(c) != 1:
            raise SystemExit(f"{coin} hits several books: {dict(c)}")
        (a,) = c
        if a not in unlisted:
            raise SystemExit(f"{coin} hits spotMeta-listed book {a}")
        out[coin] = {"book": a, "by": "oid"}
    taken = collections.Counter(v["book"] for v in out.values())
    if any(n > 1 for n in taken.values()):
        raise SystemExit("a book is hit by several coins")
    pairs = 0
    for coin, v in out.items():
        e = int(coin[1:])
        if e % 10 == 1 and f"#{e - 1}" in out:
            pairs += 1
            if out[f"#{e - 1}"]["book"] != v["book"] - 1:
                raise SystemExit(f"pair rule fails at {coin}")
    for coin in sorted(coins - set(out)):
        e = int(coin[1:])
        other, step = (f"#{e - 1}", 1) if e % 10 == 1 else (f"#{e + 1}", -1)
        if other in out and out[other]["by"] == "oid":
            a = out[other]["book"] + step
            if a in unlisted and a not in taken:
                out[coin] = {"book": a, "by": "pair"}
                taken[a] += 1
    agree = 0
    for coin, a in sorted(state.items()):
        if coin in out:
            if out[coin]["book"] != a:
                raise SystemExit(f"{coin}: state says book {a}, {out[coin]['by']} says {out[coin]['book']}")
            agree += 1
        elif coin in coins:
            if a not in unlisted or a in taken:
                raise SystemExit(f"{coin}: state book {a} is listed or already taken")
            out[coin] = {"book": a, "by": "state"}
            taken[a] += 1
    stats = {
        "coins": len(coins),
        "by_oid": sum(v["by"] == "oid" for v in out.values()),
        "by_pair": sum(v["by"] == "pair" for v in out.values()),
        "pairs_checked": pairs,
        "by_state": sum(v["by"] == "state" for v in out.values()),
        "state_agrees": agree,
        "unmapped": sorted(coins - set(out)),
    }
    json.dump({"coins": out, "stats": stats}, open(dst, "w"), indent=1, sort_keys=True)
    print(json.dumps(stats), file=sys.stderr)


if __name__ == "__main__":
    main()
