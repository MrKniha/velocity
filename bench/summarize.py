"""Summarize bench/memory.ps1 JSON files: median (min-max) per scenario and point.
Usage: python -I bench/summarize.py results/baseline.json [results/other.json ...]"""
import json, sys, statistics
for path in sys.argv[1:]:
    rows = json.load(open(path, encoding="utf-8-sig"))
    print(f"== {path}")
    keys = []
    for r in rows:
        k = (r["scenario"], r["point"])
        if k not in keys:
            keys.append(k)
    print(f"{'scenario':9} {'point':13} {'n':>2} {'procs':>5} {'wv2':>3} {'WS MiB':>18} {'privWS MiB':>18} {'commit MiB':>18} {'peak WS':>8} {'GPU':>6}")
    for k in keys:
        rs = [r for r in rows if (r["scenario"], r["point"]) == k]
        def f(name):
            v = [r[name] for r in rs if r.get(name) is not None]
            if not v: return "-"
            return f"{statistics.median(v):.0f} ({min(v):.0f}-{max(v):.0f})" if len(v) > 1 else f"{v[0]:.0f}"
        print(f"{k[0]:9} {k[1]:13} {len(rs):>2} {rs[0]['processes']:>5} {max(r['webview2'] for r in rs):>3} {f('wsMiB'):>18} {f('privWsMiB'):>18} {f('commitMiB'):>18} {f('peakWsMiB'):>8} {f('gpuMiB'):>6}")
