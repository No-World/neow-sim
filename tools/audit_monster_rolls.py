#!/usr/bin/env python3
"""审计：每个怪物的 monsterHpRng 掷点次数（真游戏构造器 vs 引擎 spawn）。

真游戏规则（AbstractMonster.setHp / 构造器）：
  - `super(NAME, ID, <const>, ...)`            → 0 次
  - `super(NAME, ID, monsterHpRng.random(a,b))` → 1 次
  - `this.setHp(a, b)` 或 `this.setHp(a)`       → 1 次（单参 = random(a,a) = nextInt(1)）
  进阶分支（if A>=9 ... else ...）只执行其一，故 setHp 计 1 次。

用法: python3 tools/audit_monster_rolls.py [--update-baseline]
"""
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
D = "/mnt/d/Users/NoWorld/Documents/source/Neow/.tools/decompiled-full"

KIND2CLS = {
    "JawWorm": "JawWorm", "Cultist": "Cultist", "LouseNormal": "LouseNormal",
    "LouseDefensive": "LouseDefensive", "FungiBeast": "FungiBeast",
    "SpikeSlimeS": "SpikeSlime_S", "AcidSlimeS": "AcidSlime_S",
    "SpikeSlimeM": "SpikeSlime_M", "AcidSlimeM": "AcidSlime_M",
    "SpikeSlimeL": "SpikeSlime_L", "AcidSlimeL": "AcidSlime_L",
    "SlaverBlue": "SlaverBlue", "SlaverRed": "SlaverRed", "GremlinNob": "GremlinNob",
    "Lagavulin": "Lagavulin", "Sentry": "Sentry", "Guardian": "TheGuardian",
    "Hexaghost": "Hexaghost", "SlimeBoss": "SlimeBoss", "GremlinWarrior": "GremlinWarrior",
    "GremlinThief": "GremlinThief", "GremlinFat": "GremlinFat",
    "GremlinTsundere": "GremlinTsundere", "GremlinWizard": "GremlinWizard",
    "GremlinLeader": "GremlinLeader", "Taskmaster": "Taskmaster", "BronzeOrb": "BronzeOrb",
    "BronzeAutomaton": "BronzeAutomaton", "TheCollector": "TheCollector", "Champ": "Champ",
    "Darkling": "Darkling", "Reptomancer": "Reptomancer", "SnakeDagger": "SnakeDagger",
    "Transient": "Transient", "OrbWalker": "OrbWalker", "SpireGrowth": "SpireGrowth",
    "Maw": "Maw", "Byrd": "Byrd", "Chosen": "Chosen",
    "ShelledParasite": "ShelledParasite", "SphericGuardian": "SphericGuardian",
    "SnakePlant": "SnakePlant", "Snecko": "Snecko", "Centurion": "Centurion",
    "Healer": "Healer", "BookOfStabbing": "BookOfStabbing", "Looter": "Looter",
    "Mugger": "Mugger", "BanditPointy": "BanditPointy", "BanditLeader": "BanditLeader",
    "BanditBear": "BanditBear", "AwakenedOne": "AwakenedOne", "Deca": "Deca",
    "Donu": "Donu", "Nemesis": "Nemesis", "TimeEater": "TimeEater",
    "WrithingMass": "WrithingMass", "CorruptHeart": "CorruptHeart",
    "SpireShield": "SpireShield", "SpireSpear": "SpireSpear",
    "Exploder": "Exploder", "Repulsor": "Repulsor", "Spiker": "Spiker",
    "TorchHead": "TorchHead",
}


def find(cls):
    for sub in ("exordium", "city", "beyond", "ending", ""):
        p = (f"{D}/com/megacrit/cardcrawl/monsters/{sub}/{cls}.java" if sub
             else f"{D}/com/megacrit/cardcrawl/monsters/{cls}.java")
        if os.path.exists(p):
            return p
    return None


def ctor_span(src, cls):
    """取参数最多的构造器（委托重载无 setHp），跨度含 super 实参。"""
    cands = list(re.finditer(rf"public {cls}\(([^)]*)\) \{{", src))
    if not cands:
        return None
    spans = []
    for m in cands:
        depth, i = 1, m.end()
        while i < len(src) and depth:
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
            i += 1
        sup = src.rfind("super(", 0, m.start())
        spans.append(src[sup if sup != -1 else m.start():i])
    # 普通遭遇用的构造器（带 setHp）；分裂用的 3/4 参构造器（newHealth）不带 setHp
    with_set = [s for s in spans if "this.setHp(" in s]
    if with_set:
        return with_set[0]
    m = max(cands, key=lambda mm: len(mm.group(1).split(",")))
    depth, i = 1, m.end()
    while i < len(src) and depth:
        if src[i] == "{":
            depth += 1
        elif src[i] == "}":
            depth -= 1
        i += 1
    sup = src.rfind("super(", 0, m.start())
    return src[sup if sup != -1 else m.start():i]


def real_rolls(cls):
    p = find(cls)
    if not p:
        return None
    span = ctor_span(open(p).read(), cls)
    if span is None:
        return None
    sup = len(re.findall(r"monsterHpRng\.random\(", span))
    has_set = bool(re.search(r"this\.setHp\(", span))
    return sup + (1 if has_set else 0)


def engine_arms():
    src = open(f"{ROOT}/crates/core/src/state.rs").read()
    sec = src[src.index("pub fn spawn(kind: MonsterKind"):]
    arms = {}
    for m in re.finditer(r"MonsterKind::(\w+) => \{", sec):
        kind = m.group(1)
        if kind in arms:
            continue
        # 括号平衡：从 `{` 起配对
        i = m.end()          # 指向 `{` 之后
        depth = 1
        while i < len(sec) and depth:
            if sec[i] == "{":
                depth += 1
            elif sec[i] == "}":
                depth -= 1
            i += 1
        arms[kind] = sec[m.end():i - 1]
    # 元组式分支（无花括号）：`MonsterKind::X => (`
    for m in re.finditer(r"MonsterKind::(\w+) => \(", sec):
        if m.group(1) in arms:
            continue
        i = m.end()
        depth = 1
        while i < len(sec) and depth:
            if sec[i] == "(":
                depth += 1
            elif sec[i] == ")":
                depth -= 1
            i += 1
        arms[m.group(1)] = sec[m.end():i - 1]
    return arms


def main():
    arms = engine_arms()
    rows, bad = [], []
    for kind, cls in sorted(KIND2CLS.items()):
        r = real_rolls(cls)
        body = arms.get(kind)
        e = (len(re.findall(r"hp_rng\.", body))
             + len(re.findall(r"set_hp_fixed\(hp_rng", body))) if body is not None else -1
        ok = body is not None and e == r
        rows.append((kind, cls, r, e, ok))
        if not ok:
            bad.append((kind, cls, r, e))

    w = max(len(k) for k, *_ in rows)
    for kind, cls, r, e, ok in rows:
        print(f"{kind:{w}s} 真={r if r is not None else '?':>2} 引擎={e:>2}  {'OK' if ok else '❌'}")
    print(f"\n审计 {len(rows)} 类：不一致 {len(bad)}")
    for b in bad:
        print("  ", b)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
