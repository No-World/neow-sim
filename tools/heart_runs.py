#!/usr/bin/env python3
"""从玩家真实对局存档（*.run）里挑选「碎心」局，并输出可用的种子字符串。

⚠️⚠️ 用途硬约束（见 docs/parity-spec.md §1.5）：玩家对局**只允许**用于
「对拍 / 回归验证」（验证引擎与真游戏一致），**绝不允许**进入训练集、
监督数据、行为克隆或评估基线。理由：项目目标是超越人类顶尖水平，
用玩家对局做模仿学习会把玩家的水平上限固化成模型天花板。

.runs 里记录了整局决策链（path_per_floor / card_choices / relics_obtained /
campfire_choices / event_choices / boss_relics / damage_taken / master_deck），
足以把真玩家的实际走法回放成 golden trace —— 不需要用启发式策略重打。

用法: python3 tools/heart_runs.py [--min-asc N] [--char X] [--min-floor N]
"""
import argparse
import glob
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from seed_conv import seed_to_str  # noqa: E402

DEFAULT_RUNS = "/mnt/d/Program Files (x86)/Steam/steamapps/common/SlayTheSpire/runs"


def load_all(runs_dir):
    out = []
    for p in glob.glob(runs_dir + "/**/*.run", recursive=True):
        try:
            d = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        d["_path"] = p
        out.append(d)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", default=DEFAULT_RUNS)
    ap.add_argument("--min-asc", type=int, default=0)
    ap.add_argument("--min-floor", type=int, default=50, help="碎心需 floor>=50")
    ap.add_argument("--char", default=None)
    ap.add_argument("--victory-only", action="store_true", default=True)
    ap.add_argument("--all", action="store_true", help="含失败局")
    args = ap.parse_args()

    rows = []
    for d in load_all(args.runs):
        ch = d.get("character_chosen") or "?"
        asc = d.get("ascension_level") or 0
        fl = d.get("floor_reached") or 0
        if args.char and ch != args.char:
            continue
        if asc < args.min_asc or fl < args.min_floor:
            continue
        if not args.all and not d.get("victory"):
            continue
        rows.append((asc, ch, fl, seed_to_str(d["seed_played"]), d))
    rows.sort(key=lambda r: (-r[0], -r[2]))

    print(f"{'asc':>3} {'char':12} {'floor':>5} {'seed':15}  Acts  path")
    for asc, ch, fl, seed, d in rows:
        acts = sorted({str(x) for x in (d.get("path_taken") or []) if x})
        print(f"{asc:>3} {ch:12} {fl:>5} {seed:15}  {'/'.join(acts):10} {d['_path'].split('/')[-1]}")
    print(f"\n共 {len(rows)} 局（floor>={args.min_floor}, asc>={args.min_asc}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
