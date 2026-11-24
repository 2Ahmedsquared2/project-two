import sys, json, gzip, collections, re
R, S, D = sys.argv[1:4]
BOOK_T = {'order','cancel','cancelByCloid','modify','batchModify'}
G = ['1-stateless','2-refprice','3-perp-margin','4-spot-balance','5-open-orders','6-abstraction','7a-scheduleCancel','7b-evm','7c-trigger-tpsl-twap-frontend','7d-validator-oracle-system','7e-account-other']
MARGIN_T={'updateLeverage','updateIsolatedMargin','topUpIsolatedOnlyMargin'}
EVM_T={'evmRawTx','evmUserModify','sendToEvmWithData'}
TRIG_T={'twapOrder','twapCancel','trailingStop'}
SYS_T={'SetGlobalAction','validatorL1UpdateReferenceOracle','VoteEthDepositAction','VoteEthFinalizedWithdrawalAction','l1ValidatorVoteBridgeDeposit','voteAbciDigest','voteL1Hash','validatorL1Status','NetChildVaultPositionsAction','multiSig','perpDeploy','outcomeDeploy','gossipPriorityBid'}
def other_g(t):
    if t=='scheduleCancel': return '7a-scheduleCancel'
    if t in MARGIN_T: return '3-perp-margin'
    if t in EVM_T: return '7b-evm'
    if t in TRIG_T: return '7c-trigger-tpsl-twap-frontend'
    if t in SYS_T: return '7d-validator-oracle-system'
    return '7e-account-other'

def cls_err(m):
    if 'tick size' in m or 'invalid price' in m or 'invalid size' in m or 'zero size' in m or 'already expired' in m: return '1-stateless'
    if 'minimum value' in m or 'away from the reference price' in m: return '2-refprice'
    if 'Insufficient margin' in m or 'Reduce only' in m or 'open interest is at cap' in m: return '3-perp-margin'
    if 'spot balance' in m or 'Invalid spot' in m: return '4-spot-balance'
    if 'Too many open orders' in m: return '5-open-orders'
    if 'abstraction operations' in m: return '6-abstraction'
    if ('Post only order would have immediately matched' in m or 'could not immediately match' in m
        or 'never placed, already canceled, or filled' in m or 'Cannot modify canceled or filled' in m):
        return None
    return '7-else'
STATUS_G = {'reduceOnlyCanceled':'3-perp-margin','marginCanceled':'3-perp-margin',
            'triggered':'7c-trigger-tpsl-twap-frontend','siblingFilledCanceled':'7c-trigger-tpsl-twap-frontend','outcomeSettledCanceled':'7e-account-other'}
def legs(a):
    t=a['action']['type']; act=a['action']
    if t=='order': return act.get('orders',[])
    if t=='modify': return [act.get('order',{})]
    if t=='batchModify': return [m.get('order',{}) for m in act.get('modifies',[])]
    return []
def act_pieces(a, res, why):
    t=a['action']['type']; ps=set()
    if res.get('status')!='ok' and t not in BOOK_T and t!='noop':
        g=other_g(t); ps.add(g); why[g][f'{t}:err']+=1; return ps
    if res.get('status')!='ok':
        g=cls_err(str(res.get('response')))
        if g=='7-else': g='7e-account-other'
        
        if g: ps.add(g); why[g][f"{t}:err:{re.sub(r'[0-9]+','<n>',str(res.get('response')))[:60]}"]+=1
        return ps
    if t=='noop': return ps
    if t not in BOOK_T:
        g=other_g(t); ps.add(g); why[g][f'non-book type {t}']+=1; return ps
    for l in legs(a):
        if l.get('r'): ps.add('3-perp-margin'); why['3-perp-margin']['reduce-only leg']+=1
        tt=l.get('t',{})
        if 'trigger' in tt: ps.add('7c-trigger-tpsl-twap-frontend'); why['7c-trigger-tpsl-twap-frontend']['trigger leg']+=1
        elif tt.get('limit',{}).get('tif') not in ('Gtc','Alo','Ioc'): ps.add('7c-trigger-tpsl-twap-frontend'); why['7c-trigger-tpsl-twap-frontend'][f"tif {tt.get('limit',{}).get('tif')}"]+=1
    if a['action'].get('grouping') not in (None,'na'): 
        g=a['action'].get('grouping')
        if not (isinstance(g,dict) and set(g)=={'p'}): ps.add('7c-trigger-tpsl-twap-frontend'); why['7c-trigger-tpsl-twap-frontend'][f'grouping {g}']+=1
    data=(res.get('response') or {}).get('data') or {}
    for st in data.get('statuses',[]):
        if isinstance(st,dict) and 'error' in st:
            g=cls_err(st['error'])
            if g=='7-else': g='7e-account-other'
            if g: ps.add(g); why[g][f"{t}:{re.sub(r'[0-9.]+','<n>',st['error'])[:60]}"]+=1
        elif isinstance(st,str) and st not in ('success',):
            ps.add('7c-trigger-tpsl-twap-frontend'); why['7c-trigger-tpsl-twap-frontend'][f'status {st}']+=1
    return ps
why=collections.defaultdict(collections.Counter)
evm_ok=set()
need={}   # height -> set of pieces
users_nonbook={}  # height -> set of users with only non-book actions
with gzip.open(R,'rt') as f:
    for line in f:
        d=json.loads(line); h=d['height']
        acts=[a for b in d['abci_block']['signed_action_bundles'] for a in b[1]['signed_actions']]
        rs=[r for b in d['resps']['Full'] for r in b[1]]
        assert len(acts)==len(rs)
        ps=set(); ub=set(); unb=set()
        if any(a['action']['type']=='evmRawTx' and r['res'].get('status')=='ok' for a,r in zip(acts,rs)): evm_ok.add(h)
        for a,r in zip(acts,rs):
            ps|=act_pieces(a,r['res'],why)
            u=(r.get('user') or '').lower()
            (ub if a['action']['type'] in BOOK_T else unb).add(u)
        need[h]=ps; users_nonbook[h]=unb-ub
with gzip.open(S,'rt') as f:
    for line in f:
        d=json.loads(line); h=d['block_number']
        for e in d['events']:
            g=STATUS_G.get(e['status'])
            if g and h in need: need[h].add(g); why[g][f"status event {e['status']} (no action behind it)"]+=1
# diff check: diffs by users whose only actions this block are non-book types
nb_diff=collections.Counter(); 
with gzip.open(D,'rt') as f:
    for line in f:
        d=json.loads(line); h=d['block_number']; u=users_nonbook.get(h,set())
        for e in d['events']:
            if e['user'].lower() in u: nb_diff[h]+=1
N=len(need)
base=sum(1 for p in need.values() if not p)
out={'blocks':N,'heights':[min(need),max(need)],'book_only_ceiling':base,'alone':{},'cumulative':{},'why':{g:why[g].most_common(12) for g in G}}
acc=set()
for g in G:
    out['alone'][g]=sum(1 for p in need.values() if p<= {g})
    acc.add(g); out['cumulative'][g]=sum(1 for p in need.values() if p<=acc)
# blocks blocked only by 7-else sub-causes
out['blocks_needing_each']={g:sum(1 for p in need.values() if g in p) for g in G}
out['nonbook_diff_check']={'blocks_with_diffs_by_users_with_only_nonbook_actions':len(nb_diff),'diff_events':sum(nb_diff.values())}
out['blocks_with_accepted_evmRawTx']=len(evm_ok)
out['book_only_blocks_with_accepted_evmRawTx']=sum(1 for h in evm_ok if need[h]<={'7b-evm'})
json.dump(out,sys.stdout,indent=1)
