#!/usr/bin/env python3
"""Build hl-account's real-data margin fixture.

Inputs (our node, pinned by sha256 in the fixture header):
  --state    translated ABCI state at the seed height, .json.zst (pretty-printed JSON)
  --replica  replica_cmds jsonl for the blocks after the seed (actions + native responses)
  --fills    node_fills_by_block jsonl.gz for the same blocks
Output: one JSON file with, for each "clean" case, the acting user's seeded account
(perp cash, positions, leverage settings, open-order aggregates, spot USDC), the
marks in force at that block, the order, and native's outcome (the answer key).

A case is clean when it is the user's first perp limit order in the window and the
user has no earlier fill, cancel, modify or non-order action in the window, so the
seeded state is exactly the state native checked against. Native's response is
only the expected value; nothing from it feeds the state.

Integer units throughout: USDC micro (1e-6) for perp cash and notionals
(px_ticks * lots), 1e-8 for spot USDC, prices in ticks of 10^-(6-szDecimals),
sizes in lots of 10^-szDecimals.
"""
import argparse, gzip, hashlib, json, re, subprocess, sys
from fractions import Fraction as F

ADDR = re.compile(r'^(\s+)"(0x[0-9a-f]{40})",\s*$')


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def read_state(path, users):
    """One streaming pass over the pretty-printed state; keeps only what the margin
    rule needs, for `users`, from the main perp dex (cls[0]) and spot (scl)."""
    p = subprocess.Popen(['zstdcat', path], stdout=subprocess.PIPE, bufsize=1 << 24)
    sec = None; cap = None; buf = []; endind = None
    out = {'uts': {}, 'oot': {}, 'spot': {}, 'meta': [], 'oracle': []}
    seen_cls0 = False; in_scl = False; done_spot = False
    for raw in p.stdout:
        s = raw.decode()
        st = s.strip()
        if cap:
            if s.startswith(endind + ']'):
                out[cap[0]][cap[1]] = json.loads(''.join(buf).rstrip().rstrip(','))
                cap = None
            else:
                buf.append(s)
            continue
        if sec in ('meta', 'oracle'):
            out[sec].append(s)
            if (sec == 'meta' and st.startswith('"collateralTokenName"')) or (sec == 'oracle' and st.startswith('"err_dur_guard"')):
                sec = None
            continue
        if not seen_cls0 and st == '"meta": {':
            sec = 'meta'; out['meta'].append(s); continue
        if not seen_cls0 and st == '"user_to_state": [':
            sec = 'uts'; continue
        if not seen_cls0 and st == '"user_to_data": [':
            sec = 'oot'; continue
        if not seen_cls0 and st == '"oracle": {':
            sec = 'oracle'; out['oracle'].append(s); seen_cls0 = True; continue
        if st == '"scl": {':
            in_scl = True
        if in_scl and not done_spot and st == '"user_states": [':
            sec = 'spot'; continue
        if sec == 'spot' and st.startswith('"daily_exchange_scaled_and_raw_vlms"'):
            done_spot = True; break
        if sec in ('uts', 'oot', 'spot'):
            m = ADDR.match(s)
            if m and m.group(2) in users:
                cap = (sec, m.group(2)); endind = m.group(1)[:-2]; buf = []
            if sec == 'uts' and st.startswith('"users_with_positions"'):
                sec = None
            if sec == 'oot' and st.startswith('"ar"'):
                sec = None
    p.kill()
    meta = json.loads('{' + ''.join(out['meta']).strip().rstrip(',') + '}}')['meta']
    orc = json.loads('{' + ''.join(out['oracle']).split('"err_dur_guard"')[0].strip().rstrip(',') + '}}')['oracle']['pxs2']
    return meta, orc, out


def max_leverage(meta, a):
    tid = meta['universe'][a]['marginTableId']
    tables = dict(meta['marginTableIdToMarginTable'])
    return tables[tid]['margin_tiers'][0]['max_leverage'] if tid in tables else tid


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--state', required=True); ap.add_argument('--replica', required=True, action='append', help='repeat for consecutive files; .gz allowed')
    ap.add_argument('--fills', required=True); ap.add_argument('--out', required=True)
    ap.add_argument('--default-leverage', type=int, default=20)
    ap.add_argument('--seed-height', type=int, default=1232430000)
    a = ap.parse_args()

    # Pass 1 over the replica: perp legs with their user, and each user's first non-order touch.
    legs = []; touch = {}; users = set(); sga = []
    def replica_lines():
        for path in a.replica:
            with (gzip.open(path, 'rb') if path.endswith('.gz') else open(path, 'rb')) as f:
                yield from f
    for raw in replica_lines():
        d = json.loads(raw); H = d['height']; rb = {h: r for h, r in d['resps']['Full']}; q = 0
        for h, bd in d['abci_block']['signed_action_bundles']:
            for i, sa in enumerate(bd['signed_actions']):
                act = sa['action']; t = act.get('type'); r = rb[h][i]; res = r['res']; u = r.get('user'); q += 1
                if t == 'SetGlobalAction':
                    sga.append((H, q, act['pxs'])); continue
                if t == 'order' and res['status'] == 'ok' and isinstance(res.get('response'), dict):
                    for j, (o, s) in enumerate(zip(act['orders'], res['response'].get('data', {}).get('statuses', []))):
                        if o['a'] < 10000:
                            legs.append((H, q, j, u, o, s)); users.add(u)
                elif t not in ('noop', 'evmRawTx') and u not in touch:
                    touch[u] = (H, q)
    first_fill = {}
    for raw in gzip.open(a.fills, 'rb'):
        d = json.loads(raw)
        for u, _ in d['events']:
            first_fill.setdefault(u, d['block_number'])

    meta, orc, st = read_state(a.state, users)
    uni = meta['universe']
    mark = {k: orc[k][1]['p'] for k in range(len(orc))}  # ticks

    def ticks(asset, px):
        return int(F(px) * 10 ** (6 - uni[asset]['szDecimals']))

    cases = []; seen = set(); gi = 0
    for (H, q, j, u, o, s) in legs:
        while gi < len(sga) and (sga[gi][0], sga[gi][1]) < (H, q):
            for k, pr in enumerate(sga[gi][2]):
                if pr and pr[0] is not None and k < len(uni):
                    mark[k] = ticks(k, pr[1])
            gi += 1
        if u in seen:
            continue
        seen.add(u)
        if j != 0 or 'limit' not in o['t'] or o['r']:
            continue
        if first_fill.get(u, 1 << 62) <= H or touch.get(u, (1 << 62, 0)) < (H, q):
            continue
        err = s.get('error', '') if isinstance(s, dict) else ''
        if err and 'Insufficient margin' not in err:
            continue
        ust = st['uts'].get(u, {})
        pos = [(k, v) for k, v in ust.get('p', {}).get('p', [])]
        if any('I' in v.get('l', {}) for k, v in pos if v.get('s')):
            continue  # isolated margin: out of scope for this rule
        positions = []; lev_settings = []
        for k, v in pos:
            lv = v.get('l', {}).get('C')
            if lv:
                lev_settings.append([k, lv])
            if v.get('s'):
                positions.append({'asset': k, 'szi': v['s'], 'entry_ntl': v.get('e', 0)})
        orders = []
        for k, v in st['oot'].get(u, {}).get('a', []):
            orders.append({'asset': k, 'bid_ntl': v['n'][0], 'ask_ntl': v['n'][1], 'bid_lots': v['s'][0],
                           'ask_lots': v['s'][1], 'leverage': v.get('l', 1)})
        spot = 0
        for tok, b in st['spot'].get(u, {}).get('b', []):
            if tok == 0:
                spot = b.get('t', 0) - b.get('h', 0)
        assets = sorted({p['asset'] for p in positions} | {o['a']})
        cases.append({
            'height': H, 'user': u,
            'account': {'usdc': ust.get('u', 0), 'spot_usdc': spot, 'positions': positions,
                        'leverage': lev_settings, 'orders': orders},
            'marks': [[k, mark[k]] for k in assets],
            'order': {'asset': o['a'], 'is_buy': o['b'], 'px_ticks': ticks(o['a'], o['p']),
                      'lots': int(F(o['s']) * 10 ** uni[o['a']]['szDecimals']), 'tif': o['t']['limit']['tif']},
            'native': 'insufficient_margin' if err else 'accepted',
        })
    markets = [{'asset': k, 'sz_decimals': uni[k]['szDecimals'], 'max_leverage': max_leverage(meta, k)}
               for k in range(len(uni))]
    out = {'source': 'our node; seed = translated ABCI state; native responses are the answer key only',
           'state_sha256': sha256(a.state), 'replica_sha256': [sha256(r) for r in a.replica], 'fills_sha256': sha256(a.fills),
           'default_leverage': a.default_leverage, 'markets': markets, 'cases': cases}
    json.dump(out, open(a.out, 'w'), separators=(',', ':'))
    print(f'{len(cases)} clean cases, {sum(c["native"] != "accepted" for c in cases)} native rejections', file=sys.stderr)


if __name__ == '__main__':
    main()
