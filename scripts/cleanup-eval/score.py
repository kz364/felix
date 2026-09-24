import json, sys, collections
from checks import run
rows = json.load(open(f"rows_{sys.argv[1]}.json"))
agg = collections.defaultdict(lambda: [0, 0, 0])
for r in rows:
    k = f"{r['prompt']}:{r['level']}"
    f = run(r["item"], r["level"], r["output"])
    agg[k][0] += 1; agg[k][1] += (not f); agg[k][2] += bool(r["guard"])
    if f and "-v" in sys.argv: print(f"  {k} {r['item']}: {f}")
for k, (n, ok, g) in agg.items(): print(f"{k:16} pass {ok}/{n}  guard_rejects {g}")
