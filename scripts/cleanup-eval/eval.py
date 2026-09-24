#!/usr/bin/env python3
"""Run cleanup prompt variants through Apple's on-device model.
   eval.py <tag> <prompt>:<level>[,...] [--plain]"""
import json, subprocess, sys, os, statistics, importlib
HERE = os.path.dirname(os.path.abspath(__file__)); os.chdir(HERE)
import prompts; importlib.reload(prompts)
tag = sys.argv[1]; specs = sys.argv[2].split(","); plain = "--plain" in sys.argv
items = json.load(open("items.json"))
jobs, meta = [], {}
for spec in specs:
    p, level = spec.split(":")
    for it in items:
        jid = f"{p}:{level}:{it['id']}"
        jobs.append({"id": jid, "system": prompts.PROMPTS[p](level, it["cat"]), "user": prompts.USER.get(p, prompts.DEFAULT_USER).format(t=it["text"])})
        meta[jid] = {"prompt": p, "level": level, "cat": it["cat"], "item": it["id"], "input": it["text"]}
json.dump(jobs, open(f"jobs_{tag}.json", "w"))
if "--reuse" not in sys.argv: subprocess.run(["./runner", f"jobs_{tag}.json", f"out_{tag}.json"] + [a for a in sys.argv if a in ("--plain","--greedy")], check=True)
outs = json.load(open(f"out_{tag}.json"))
META = ("here is", "here's", "sure", "certainly", "i'm sorry", "i cannot", "as an ai", "cleaned")
rows = []
for o in outs:
    m = meta[o["id"]]; out = o["output"].strip()
    iw, ow = max(len(m["input"].split()), 1), len(out.split())
    ratio = ow / iw
    guard = "empty" if not out else "meta" if out.lower().startswith(META) else "short" if ratio < (0.1 if iw < 8 else 0.35) else "long" if (ratio > 1.5 and ow - iw > 4) else ""
    rows.append({**m, "id": o["id"], "output": out, "ms": o["ms"], "guard": guard, "error": o.get("error")})
json.dump(rows, open(f"rows_{tag}.json", "w"), indent=1)
for spec in specs:
    rs = [r for r in rows if f"{r['prompt']}:{r['level']}" == spec]
    ms = [r["ms"] for r in rs]
    print(f"{spec:14} n={len(rs)} median {statistics.median(ms):.0f}ms p90 {sorted(ms)[int(.9*len(ms))-1]}ms guard_rejects={sum(1 for r in rs if r['guard'])} errors={sum(1 for r in rs if r['error'])}")
