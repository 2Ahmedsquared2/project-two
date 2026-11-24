#!/usr/bin/env python3
"""Per-leg margin verdicts over an evolving account state (our node's data).

Evolves main-dex perp accounts from the seed snapshot through the replica blocks in
bundle order. At every main-dex perp order
leg (order, modify, batchModify) the margin verdict is decided from the evolved state
BEFORE that leg; native's answer to that leg is only the answer key. Inside each bundle the
actions run in Hyperliquid's documented three-class order (see exec_class).

State inputs (read, and labelled as such in every report):
  seed     translated ABCI state at the seed height (cash u, positions, leverage settings,
           spot USDC t-h) for the users that act or fill in the window (seed_state.json,
           built with build_margin_fixture.read_state) + the seed resting orders (l4so).
  effects  native's per-leg response (resting oid / filled / error) and the fills stream,
           attached to the leg by taker oid and tid. These are native outputs used as state
           inputs (what our engine would emit). Fills not attached to a leg (TWAP slices,
           liquidations, triggers) are applied at the end of their block.
  statuses (only where an order-status stream exists) the new oid of a single `modify`
           and cancels not caused by an action, applied at the end of their block.
Flagged, not dropped: isolated margin, users touched by an unmodelled transfer
(`dirty`), trigger and reduce-only legs. Funding is not modelled.

Output: one JSON line per main-dex perp leg:
  [height, user, action type, asset, is_buy, tif, kind, native, ours(spot counted),
   ours(spot not counted), not-modelled reason, user seeded, free-minus-need micro-USDC,
   abstraction (state ust field a: u = unified, i, p; null = no entry)]
"""
import argparse, collections, gzip, json, sys
from fractions import Fraction as F

import orjson

SPOT_PER_MICRO = 100
DIRTY = {'usdSend', 'sendAsset', 'spotSend', 'subAccountTransfer', 'subAccountSpotTransfer', 'vaultTransfer',
         'updateIsolatedMargin', 'cDeposit', 'cWithdraw', 'borrowLend',
         'agentSendAsset', 'withdraw3', 'createSubAccount', 'convertToMultiSigUser', 'sendToEvmWithData',
         'NetChildVaultPositionsAction', 'evmUserModify', 'claimRewards', 'tokenDelegate'}


def sc(s, k):
    """Decimal string times 10^k as an exact integer (floored past k digits)."""
    s = str(s); neg = s.startswith('-'); s = s.lstrip('-')
    w, _, f = s.partition('.')
    f2 = f.ljust(k, '0')
    v = int((w or '0') + f2[:k])
    if f2[k:].strip('0'):
        v = int(F(s) * 10 ** k)
    return -v if neg else v


def side_charge(ntl, lots, free):
    if lots <= free:
        return 0
    if free <= 0:
        return ntl
    return ntl * (lots - free) // lots


def asset_om(g, szi, lev):
    bf = 2 * -szi if szi < 0 else 0
    af = 2 * szi if szi > 0 else 0
    return max(side_charge(g[0], g[2], bf), side_charge(g[1], g[3], af)) // max(lev, 1)


def exec_class(act):
    """Hyperliquid's in-bundle order (docs, Order book; as lane 1's hl-exec `exec_class`):
    0 = sends no GTC/IOC leg (ALO orders and modifies, everything else), 1 = cancels,
    2 = sends at least one non-ALO leg (GTC, IOC, trigger)."""
    t = act.get('type')
    if t in ('cancel', 'cancelByCloid'):
        return 1
    if t == 'order':
        legs = act['orders']
    elif t == 'batchModify':
        legs = [m['order'] for m in act['modifies']]
    elif t == 'modify':
        legs = [act['order']]
    else:
        return 0
    return 2 if any(o['t'].get('limit', {}).get('tif') != 'Alo' for o in legs) else 0


class Acc:
    __slots__ = ('u', 'spot', 'pos', 'lev', 'iso', 'agg', 'olev', 'dirty', 'seeded', 'abs')

    def __init__(self):
        self.u = 0; self.spot = 0; self.pos = {}; self.lev = {}; self.iso = set(); self.agg = {}; self.olev = {}
        self.dirty = None; self.seeded = False; self.abs = None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--seed-state', required=True)
    ap.add_argument('--seed-orders', required=True, help='l4so json at the seed height')
    ap.add_argument('--spot-meta', required=True)
    ap.add_argument('--replica', required=True, action='append')
    ap.add_argument('--fills', required=True, help='node_fills_by_block jsonl.gz covering every replica block')
    ap.add_argument('--statuses', help='node_order_statuses_by_block jsonl for a prefix of the blocks')
    ap.add_argument('--abstraction', help='json user -> state ust entry (field a: u/i/p) at the seed height')
    ap.add_argument('--end-orders', help='l4so json at the last replica height: end-state check of resting orders')
    ap.add_argument('--end-state', help='json {uts: user -> user_to_state} at the last replica height: end-state check of cash and positions')
    ap.add_argument('--default-leverage', type=int, default=20)
    ap.add_argument('--out', required=True)
    a_ = ap.parse_args()

    S = json.load(open(a_.seed_state))
    meta = S['meta']; uni = meta['universe']
    name2a = {u['name']: i for i, u in enumerate(uni)}
    szd = [u['szDecimals'] for u in uni]
    tables = dict(meta['marginTableIdToMarginTable'])

    def maxlev(a):
        tid = uni[a]['marginTableId']
        return tables[tid]['margin_tiers'][0]['max_leverage'] if tid in tables else tid
    maxl = [maxlev(a) for a in range(len(uni))]
    spotq = {}
    sm = json.load(open(a_.spot_meta))
    for p in sm['universe']:
        spotq[p['name']] = p['tokens'][1]; spotq['@%d' % p['index']] = p['tokens'][1]
    spot_name = {p['index']: p['name'] for p in sm['universe']}
    mark = {k: S['oracle'][k][1]['p'] for k in range(len(uni))}

    A = collections.defaultdict(Acc)
    for u, st in S['uts'].items():
        acc = A[u]; acc.seeded = True; acc.u = st.get('u', 0)
        for k, v in st.get('p', {}).get('p', []):
            l = v.get('l', {})
            if 'C' in l: acc.lev[k] = l['C']
            if 'I' in l: acc.lev[k] = l['I']['l']; acc.iso.add(k)
            if v.get('s'): acc.pos[k] = v['s']
    for u, st in S['spot'].items():
        acc = A[u]; acc.seeded = True
        for tok, b in st.get('b', []):
            if tok == 0: acc.spot = b.get('t', 0) - b.get('h', 0)
    if a_.abstraction:
        for u, v in json.load(open(a_.abstraction)).items():
            A[u].abs = v.get('a')
    seed_oot = {}
    for u, st in S['oot'].items():
        for k, v in st.get('a', []):
            A[u].olev[k] = v.get('l', 1); seed_oot[(u, k)] = v

    def levof(acc, a):
        if a in acc.lev: return max(acc.lev[a], 1)
        if a in acc.olev: return max(acc.olev[a], 1)
        return max(min(a_.default_leverage, maxl[a]), 1)

    ORD = {}; CLOID = {}

    def agg_add(u, a, is_buy, ntl, lots):
        g = A[u].agg.setdefault(a, [0, 0, 0, 0])
        if is_buy: g[0] += ntl; g[2] += lots
        else: g[1] += ntl; g[3] += lots
        if g == [0, 0, 0, 0]: del A[u].agg[a]

    def spot_hold(coin, is_buy, px, sz):
        if is_buy and spotq.get(coin) == 0:
            return int(F(px) * F(sz) * 10 ** 8)
        return 0

    def add_order(oid, u, coin, is_buy, px, sz, cloid, seed):
        a = name2a.get(coin)
        if a is not None:
            pt = sc(px, 6 - szd[a]); lots = sz if isinstance(sz, int) else sc(sz, szd[a])
            if lots <= 0: return
            ORD[oid] = [u, coin, a, is_buy, px, pt, lots]
            agg_add(u, a, is_buy, pt * lots, lots)
            if a not in A[u].olev: A[u].olev[a] = levof(A[u], a)
        else:
            q = F(sz)
            if q <= 0: return
            ORD[oid] = [u, coin, None, is_buy, px, 0, q]
            if not seed: A[u].spot -= spot_hold(coin, is_buy, px, q)
        if cloid: CLOID[(u, cloid)] = oid

    def reduce_order(oid, amt, release_hold=True):
        o = ORD.get(oid)
        if o is None: return False
        u, coin, a, is_buy, px, pt, rem = o
        if a is not None:
            d = min(amt, rem); agg_add(u, a, is_buy, -pt * d, -d)
        else:
            d = min(F(amt), rem)
            if release_hold: A[u].spot += spot_hold(coin, is_buy, px, d)
        o[6] = rem - d
        if o[6] <= 0: del ORD[oid]
        return True

    def cancel(oid):
        o = ORD.get(oid)
        return False if o is None else reduce_order(oid, o[6])

    l4 = json.load(open(a_.seed_orders))
    for coin, sides in l4[1]:
        for side in sides:
            for u, o in side:
                add_order(o['oid'], u, coin, o['side'] == 'B', o['limitPx'], o['sz'], o.get('cloid'), True)
    agg_chk = collections.Counter()
    for (u, k), v in seed_oot.items():
        g = A[u].agg.get(k, [0, 0, 0, 0])
        agg_chk['equal' if g == [v['n'][0], v['n'][1], v['s'][0], v['s'][1]] else 'differ'] += 1

    def check(u, a, is_buy, pt, lots, use_spot):
        acc = A[u]; lev = levof(acc, a); szi = acc.pos.get(a, 0)
        before = acc.agg.get(a, [0, 0, 0, 0]); after = list(before)
        if is_buy: after[0] += pt * lots; after[2] += lots
        else: after[1] += pt * lots; after[3] += lots
        need = asset_om(after, szi, lev) - asset_om(before, szi, acc.olev.get(a, lev))
        if need <= 0: return 'accept', 0
        value = acc.u; im = 0
        for k, s in acc.pos.items():
            ntl = s * mark[k]; value += ntl; im += abs(ntl) // levof(acc, k)
        om = sum(asset_om(g, acc.pos.get(k, 0), acc.olev.get(k, levof(acc, k))) for k, g in acc.agg.items())
        free = (value - im - om) * SPOT_PER_MICRO + (acc.spot if use_spot else 0)
        return ('accept' if free >= need * SPOT_PER_MICRO else 'margin'), free - need * SPOT_PER_MICRO

    cnt = collections.Counter(); posbad = []

    def apply_fill(u, f):
        coin = f['coin']; a = name2a.get(coin); acc = A[u]; buy = f['side'] == 'B'
        fee = f.get('fee', '0') or '0'
        if a is not None:
            lots = sc(f['sz'], szd[a]); pt = sc(f['px'], 6 - szd[a])
            sp = round(F(f['startPosition']) * 10 ** szd[a]); cur = acc.pos.get(a, 0)  # written via a float
            cnt['fill: our position == startPosition' if sp == cur else 'fill: POSITION DIFFERS'] += 1
            if sp != cur and len(posbad) < 8: posbad.append((u, coin, cur, sp, f.get('dir')))
            new = sp + (lots if buy else -lots)   # continue from native's startPosition (read)
            if new: acc.pos[a] = new
            else: acc.pos.pop(a, None)
            acc.u -= (lots if buy else -lots) * pt
            acc.u -= sc(fee, 6) + sc(f.get('builderFee') or '0', 6)
            if not f.get('crossed'): reduce_order(f['oid'], lots)
        elif ':' not in coin:
            q = F(f['px']) * F(f['sz']) * 10 ** 8
            if spotq.get(coin) == 0:
                if buy:
                    if f.get('crossed'): acc.spot -= int(q)
                    else: reduce_order(f['oid'], F(f['sz']), release_hold=False)
                else:
                    acc.spot += int(q)
                    if not f.get('crossed'): reduce_order(f['oid'], F(f['sz']))
            elif not f.get('crossed'):
                reduce_order(f['oid'], F(f['sz']))
            if (f.get('feeToken') or 'USDC') == 'USDC': acc.spot -= sc(fee, 8)
        elif not f.get('crossed'):
            reduce_order(f['oid'], F(f['sz']))

    def mark_dirty(u, why, H):
        if u and A[u].dirty is None: A[u].dirty = (why, H)

    def rep_lines():
        for p in a_.replica:
            yield from (gzip.open(p, 'rb') if p.endswith('.gz') else open(p, 'rb'))
    fills_f = gzip.open(a_.fills, 'rb')
    os_f = open(a_.statuses, 'rb') if a_.statuses else None
    out = gzip.open(a_.out, 'wt')
    for raw in rep_lines():
        d = orjson.loads(raw); H = d['height']
        fb = orjson.loads(fills_f.readline())
        assert fb['block_number'] == H, (fb['block_number'], H)
        ob = None
        if os_f:
            line = os_f.readline()
            if line:
                ob = orjson.loads(line); assert ob['block_number'] == H
            else:
                os_f = None
        bytid = {}; taker_tids = collections.defaultdict(list)
        for u, f in fb['events']:
            bytid.setdefault(f['tid'], []).append((u.lower(), f))
            if f.get('crossed'): taker_tids[f['oid']].append(f['tid'])
        done_tid = set(); pending = set()

        def fills_for(oid):
            got = 0
            for t in taker_tids.get(oid, []):
                if t in done_tid: continue
                done_tid.add(t)
                for uu, f in bytid[t]:
                    if f.get('crossed'): got += 1
                    apply_fill(uu, f)
            return got
        st_open = collections.defaultdict(list)
        if ob:
            for e in ob['events']:
                o = e['order']
                if e['status'] in ('open', 'filled'):
                    st_open[(e['user'].lower(), o['coin'], o['side'] == 'B', o['limitPx'])].append(o['oid'])
        rb = {h: r for h, r in d['resps']['Full']}
        for h, bd in d['abci_block']['signed_action_bundles']:
            for i, sa in sorted(enumerate(bd['signed_actions']), key=lambda x: (exec_class(x[1]['action']), x[0])):
                act = sa['action']; t = act.get('type'); r = rb[h][i]; res = r['res']
                u = (r.get('user') or '').lower()
                resp = res.get('response') if isinstance(res.get('response'), dict) else {}
                dat = resp.get('data') if resp else None
                sts = dat.get('statuses', []) if isinstance(dat, dict) else []
                if t == 'SetGlobalAction':
                    for k, pr in enumerate(act['pxs']):
                        if pr and pr[0] is not None and k < len(uni): mark[k] = sc(pr[1], 6 - szd[k])
                    continue
                if res.get('status') != 'ok': continue
                if t in ('order', 'batchModify', 'modify'):
                    if t == 'order': legs = [(None, o) for o in act['orders']]
                    elif t == 'batchModify': legs = [(m['oid'], m['order']) for m in act['modifies']]
                    else: legs = [(act['oid'], act['order'])]
                    for j, (old, o) in enumerate(legs):
                        s = sts[j] if j < len(sts) else None
                        a = o['a']; buy = o['b']
                        if old is not None:
                            oo = CLOID.get((u, old)) if isinstance(old, str) else old
                            if oo is None or not cancel(oo):
                                cnt['modify: old order not held, ' + ('main perp' if a < 10000 else ('spot' if a < 100000 else 'HIP-3')) + ' (' + ('accepted' if isinstance(s, dict) and ('resting' in s or 'filled' in s) else 'rejected/none') + ')'] += 1
                        if a < 10000:
                            lots = sc(o['s'], szd[a]); pt = sc(o['p'], 6 - szd[a])
                            if isinstance(s, dict) and 'error' in s:
                                nat = 'margin' if 'Insufficient margin' in s['error'] else 'other:' + s['error'].split('.')[0][:60]
                            elif isinstance(s, dict) and ('resting' in s or 'filled' in s): nat = 'accept'
                            elif s is None and t == 'modify': nat = 'modify-noresp'
                            else: nat = 'other:' + str(s)[:30]
                            kind = 'trigger' if 'trigger' in o['t'] else ('ro' if o['r'] else 'plain')
                            acc = A[u]
                            nm = 'isolated' if acc.iso else ('dirty:' + acc.dirty[0] if acc.dirty else None)
                            v1, m1 = check(u, a, buy, pt, lots, True)
                            v0, _ = check(u, a, buy, pt, lots, False)
                            tif = o['t']['limit'].get('tif') if 'limit' in o['t'] else 'trig'
                            out.write(orjson.dumps([H, u, t, a, buy, tif, kind, nat, v1, v0, nm, acc.seeded,
                                                    int(m1 // SPOT_PER_MICRO), acc.abs]).decode() + '\n')
                        oid = None
                        if isinstance(s, dict):
                            if 'resting' in s: oid = s['resting']['oid']
                            elif 'filled' in s: oid = s['filled']['oid']
                        elif t == 'modify':
                            coin = uni[a]['name'] if a < 10000 else (spot_name.get(a - 10000) if a < 100000 else None)
                            cands = st_open.get((u, coin, buy, o['p'])) if (ob and coin) else None
                            if cands: oid = cands.pop(0)
                            else:
                                cnt['modify: new oid unresolved, ' + ('main perp' if a < 10000 else ('spot' if a < 100000 else 'HIP-3')) + ('' if ob else ' (no status stream)')] += 1
                        if oid is None: continue
                        nf = fills_for(oid)
                        rests = (isinstance(s, dict) and 'resting' in s) or (t == 'modify')
                        if not rests or 'trigger' in o['t']: continue
                        if (u, 'o', oid) in pending or (o.get('c') and (u, 'c', o['c']) in pending):
                            cnt['pending cancel applied on placement'] += 1
                            pending.discard((u, 'o', oid)); pending.discard((u, 'c', o.get('c')))
                            continue
                        if a < 10000:
                            coin = uni[a]['name']
                            filled = sum(sc(f['sz'], szd[a]) for tt in taker_tids.get(oid, []) for _, f in bytid[tt] if f.get('crossed'))
                            add_order(oid, u, coin, buy, o['p'], lots - filled, o.get('c'), False)
                        elif a < 100000:
                            coin = spot_name.get(a - 10000)
                            filled = sum(F(f['sz']) for tt in taker_tids.get(oid, []) for _, f in bytid[tt] if f.get('crossed'))
                            add_order(oid, u, coin, buy, o['p'], F(o['s']) - filled, o.get('c'), False)
                        else:
                            if o.get('c'): CLOID[(u, o['c'])] = oid
                elif t in ('cancel', 'cancelByCloid'):
                    for c, s in zip(act['cancels'], sts):
                        if s != 'success': continue
                        oid = c.get('o') if t == 'cancel' else CLOID.get((u, c.get('cloid')))
                        main = c.get('a', c.get('asset', 0)) < 10000
                        if oid is None or not cancel(oid):
                            # should not happen once bundles run in exec_class order; kept as a counter
                            pending.add((u, 'o', c['o']) if t == 'cancel' else (u, 'c', c.get('cloid')))
                            cnt['cancel: target not held yet -> pending (' + ('main perp' if main else 'spot/HIP-3') + ')'] += 1
                elif t == 'updateLeverage':
                    A[u].lev[act['asset']] = act['leverage']
                    if act['isCross']: A[u].iso.discard(act['asset'])
                    else: A[u].iso.add(act['asset'])
                elif t in ('userSetAbstraction', 'agentSetAbstraction'):
                    A[u].abs = act.get('abstraction')   # native accepted it (read)
                elif t == 'usdClassTransfer':
                    if 'subaccount' in str(act['amount']): mark_dirty(u, t, H); continue
                    m = sc(str(act['amount']).split(' ')[0], 6)
                    if act['toPerp']: A[u].u += m; A[u].spot -= m * SPOT_PER_MICRO
                    else: A[u].u -= m; A[u].spot += m * SPOT_PER_MICRO
                elif t in DIRTY:
                    mark_dirty(u, t, H)
                    dst = act.get('destination') or act.get('subAccountUser') or act.get('vaultAddress')
                    if isinstance(dst, str): mark_dirty(dst.lower(), t + '(dest)', H)
        cnt['pending cancels unmatched at end of block'] += len(pending)
        for tid, fl in bytid.items():
            if tid in done_tid: continue
            cnt['fills applied at end of block (no leg)'] += 1
            for u, f in fl: apply_fill(u, f)
        if ob:
            for e in ob['events']:
                stt = e['status']
                if stt == 'canceled' or stt.endswith('Canceled'):
                    if cancel(e['order']['oid']): cnt['status-stream cancel applied at end of block'] += 1
    out.close()
    if a_.end_state:
        es = json.load(open(a_.end_state))['uts']; ec = collections.Counter(); diffs = []
        for u, st in es.items():
            if u not in A: continue
            acc = A[u]
            pos = {k: v['s'] for k, v in st.get('p', {}).get('p', []) if v.get('s')}
            cu = 'cash equal' if acc.u == st.get('u', 0) else 'cash differs'
            cp = 'positions equal' if pos == acc.pos else 'positions differ'
            ec[(cu, cp, 'dirty' if acc.dirty else 'clean', 'iso' if acc.iso else 'cross')] += 1
            if cu != 'cash equal' or cp != 'positions equal':
                diffs.append([u, acc.u, st.get('u', 0), acc.u - st.get('u', 0), {k: (acc.pos.get(k), pos.get(k)) for k in set(pos) | set(acc.pos) if acc.pos.get(k) != pos.get(k)}, acc.dirty, sorted(acc.iso)])
        print('END-STATE cash/positions (users in end extract):', sorted(ec.items(), key=str), file=sys.stderr)
        json.dump(diffs, open(a_.out + '.endcash.json', 'w'))
    if a_.end_orders:
        e4 = json.load(open(a_.end_orders)); want = {}; woids = {}
        for coin, sides in e4[1]:
            k = name2a.get(coin)
            if k is None: continue
            for side in sides:
                for u, o in side:
                    g = want.setdefault((u, k), [0, 0, 0, 0]); pt = sc(o['limitPx'], 6 - szd[k]); lots = sc(o['sz'], szd[k])
                    if o['side'] == 'B': g[0] += pt * lots; g[2] += lots
                    else: g[1] += pt * lots; g[3] += lots
                    woids[o['oid']] = (u, k)
        have = {(u, k): g for u, acc in A.items() for k, g in acc.agg.items()}
        ec = collections.Counter(); bad = collections.Counter()
        for key in set(want) | set(have):
            w = want.get(key, [0, 0, 0, 0]); h = have.get(key, [0, 0, 0, 0])
            r = 'equal' if w == h else ('ours higher' if h[0] + h[1] > w[0] + w[1] else 'ours lower')
            ec[r] += 1
            if r != 'equal': bad[key[0]] += 1
        held = {oid for oid, o in ORD.items() if o[2] is not None}
        print('END-STATE resting main-perp orders, per (user, asset) aggregate vs end snapshot:', dict(ec), file=sys.stderr)
        print('END-STATE oids: native rests', len(woids), 'we hold', len(held), 'both', len(held & set(woids)), file=sys.stderr)
        json.dump({'by_user': bad.most_common(), 'only_ours': [[oid] + ORD[oid][:6] for oid in list(held - set(woids))[:20000]],
                   'only_native': [[oid, *woids[oid]] for oid in list(set(woids) - held)[:20000]]},
                  open(a_.out + '.endstate.json', 'w'))
    print('seed order aggregates vs state open_order_tracker:', dict(agg_chk), file=sys.stderr)
    print('position mismatches (first 8):', posbad, file=sys.stderr)
    for k, v in sorted(cnt.items()): print(v, k, file=sys.stderr)


if __name__ == '__main__':
    main()
