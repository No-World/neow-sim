#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
确定性审计：扫描反编译源码中所有潜在非确定性来源，
按包分类（游戏性相关 / 表现层），输出登记表供 docs/parity-spec.md 使用。

用法: python3 audit_determinism.py <decompiled_dir> <out_json>
"""
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

PATTERNS = {
    "mathutils_random": r"MathUtils\.random\s*\(",
    "collections_shuffle": r"Collections\.shuffle\s*\(",
    "new_java_util_random": r"new\s+java\.util\.Random\s*\(",
    "new_randomxs128_unseeded": r"new\s+RandomXS128\s*\(\s*\)",
    "current_time": r"System\.(currentTimeMillis|nanoTime)\s*\(",
}

GAMEPLAY_PREFIX = "com/megacrit/cardcrawl/"
COSMETIC_PARTS = ("/vfx/", "/ui/", "/screens/", "/helpers/input", "/helpers/controller",
                  "/helpers/font", "/audio/", "/fx", "/hierarchy", "/localization")


def classify(path_str: str) -> str:
    p = path_str.replace("\\", "/")
    if not p.startswith(GAMEPLAY_PREFIX):
        return "external(libgdx/twitch等)"
    if any(part in p for part in COSMETIC_PARTS):
        return "cosmetic"
    return "gameplay"


def main():
    root = Path(sys.argv[1])
    out = Path(sys.argv[2])
    hits = defaultdict(list)
    stats = defaultdict(lambda: defaultdict(int))

    for f in root.rglob("*.java"):
        rel = str(f.relative_to(root))
        pkg = rel.split("/")[0]
        if pkg not in ("com",):  # 只审计游戏与gdx的数学层
            continue
        text = f.read_text(errors="replace")
        for name, pat in PATTERNS.items():
            for i, line in enumerate(text.splitlines(), 1):
                if re.search(pat, line):
                    entry = {"file": rel, "line": i, "code": line.strip()[:160]}
                    hits[name].append(entry)
                    stats[name][classify(rel)] += 1

    report = {"meta": {"note": "确定性审计：gameplay-suspect 项必须在 parity-spec 登记处置"},
              "stats": {k: dict(v) for k, v in stats.items()},
              "hits": dict(hits)}
    out.write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding="utf-8")

    print(f"{'来源':28s} {'游戏性':8s} 表现层    外部库")
    for name in PATTERNS:
        s = stats[name]
        print(f"{name:28s} {s.get('gameplay', 0):<8d} {s.get('cosmetic', 0):<9d} {s.get('external(libgdx/twitch等)', 0)}")
    print("\n== gameplay 明细（前 50 条）==")
    n = 0
    for name, lst in hits.items():
        for h in lst:
            if classify(h["file"]) == "gameplay" and n < 50:
                print(f"[{name}] {h['file']}:{h['line']}  {h['code'][:100]}")
                n += 1


if __name__ == "__main__":
    main()
