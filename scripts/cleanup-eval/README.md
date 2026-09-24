# Cleanup prompt eval (Apple Intelligence)

Tunes the AI cleanup prompts in `src-tauri/src/cleanup.rs` against Apple's
on-device model, called exactly like Handy's Swift bridge.

```bash
cd scripts/cleanup-eval
swiftc -O -parse-as-library runner.swift -o runner
python3 eval.py r1 p4:light,p4:medium   # run prompt variants (prompts.py)
python3 score.py r1 -v                  # deterministic per-item checks (checks.py)
```

- `items.json`: 32 dictations across personal / work / email / other,
  including instructions that must not be executed.
- `checks.py`: must-contain / must-not-contain checks per item and level.
- `retention.py`, `numguard.py`: prototypes of the app's output guard
  (content retention, novelty, invented numbers).
- `prompts.py`: all variants tried; `p4` is what ships. `p0` is the previous
  default.

Final comparison was blind-judged by Claude Sonnet (fidelity / cleanliness /
level fit / hard failures, 1–5): old default 4.31 / 4.28 / 4.19 with 5 hard
failures; new Light 4.66 / 4.59 / 4.50 with 2; new Medium 4.47 / 4.66 / 4.41
with 2.
