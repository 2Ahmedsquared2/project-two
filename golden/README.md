# Golden vector

`basic.jsonl` is a hand-built six-block dataset for the asset `GOLD`
(`px_decimals` 1, `sz_decimals` 2). The expected book after each block was
derived by hand below, and `book-check` must replay it cleanly through the
reference book.

| block | ops                                             | bids (px: queue)                   | asks (px: queue)            |
|-------|-------------------------------------------------|------------------------------------|-----------------------------|
| 1     | insert 1 bid 1000×500; insert 2 ask 1010×300    | 1000: [1×500]                      | 1010: [2×300]               |
| 2     | insert 3 bid 1000×200; insert 4 bid 995×100     | 1000: [1×500, 3×200]; 995: [4×100] | 1010: [2×300]               |
| 3     | resize 1→150; insert 5 ask 1010×50              | 1000: [1×150, 3×200]; 995: [4×100] | 1010: [2×300, 5×50]         |
| 4     | cancel 1; cancel 2                              | 1000: [3×200]; 995: [4×100]        | 1010: [5×50]                |
| 5     | insert 6 ask 1005×75; cancel 4                  | 1000: [3×200]                      | 1005: [6×75]; 1010: [5×50]  |
| 6     | (empty block)                                   | 1000: [3×200]                      | 1005: [6×75]; 1010: [5×50]  |

Each corrupted copy differs from `basic.jsonl` in exactly one recorded book
record (see `diff`), and `crates/book-check/tests/golden.rs` asserts the
checker reports exactly that block, side, price and field:

| file                        | corruption                                          | expected report                                          |
|-----------------------------|-----------------------------------------------------|----------------------------------------------------------|
| `basic-corrupt-size.jsonl`  | block 4, bid 1000 recorded as 250 lots (truth 200)  | block 4, bid, 1000, size: recorded 250, candidate 200     |
| `basic-corrupt-queue.jsonl` | block 2, bid 1000 queue recorded as [3, 1]          | block 2, bid, 1000, queue position 0: recorded 3, cand. 1 |
| `basic-corrupt-level.jsonl` | block 5, recorded asks lack the 1005 level          | block 5, ask, 1005, level not in recorded book            |

Run them yourself:

```bash
cargo run -q -p book-check -- golden/basic.jsonl              # exit 0
cargo run -q -p book-check -- golden/basic-corrupt-size.jsonl # exit 1
```
