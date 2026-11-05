#!/usr/bin/env python3
"""审计：每个遭遇（getEncounter）的 miscRng 掷点次数（真游戏 vs 引擎 resolve_encounter）。

规则（MonsterHelper）：每组 `miscRng.random/randomBoolean` 都是 1 次消耗，
**未被选中的候选怪也要构造**（其 monsterHpRng 掷点照旧），因此候选生成顺序与
数量都必须镜像。
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
D = "/mnt/d/Users/NoWorld/Documents/source/Neow/.tools/decompiled-full"
MHELPER = f"{D}/com/megacrit/cardcrawl/helpers/MonsterHelper.java"


def real_encounters():
    """getEncounter 的全部 case → 该分支源码（含辅助函数展开）。"""
    src = open(MHELPER).read()
    body = src[src.index("public static MonsterGroup getEncounter(String key)"):]
    out = {}
    for m in re.finditer(r'case "([^"]+)":\s*\{', body):
        name = m.group(1)
        i = m.end() - 1
        depth, j = 1, i + 1
        while j < len(body) and depth:
            if body[j] == "{":
                depth += 1
            elif body[j] == "}":
                depth -= 1
            j += 1
        out.setdefault(name, body[i:j])
    return out


def helper_src(name):
    src = open(MHELPER).read()
    for pat in (rf"private static MonsterGroup {name}\(", rf"private static AbstractMonster {name}\("):
        m = re.search(pat, src)
        if not m:
            continue
        i = src.index("{", m.end() - 1)
        depth, j = 1, i + 1
        while j < len(src) and depth:
            if src[j] == "{":
                depth += 1
            elif src[j] == "}":
                depth -= 1
            j += 1
        return src[i:j]
    return ""


def real_misc_rolls(text, depth=0, stack=()):
    """递归计权：直接 miscRng 调用 + 每个 MonsterHelper.X( 调用点 × X 内部消耗。
    同一 helper 被调用 n 次就乘 n（如 2 Louse 两次 getLouse）。"""
    if depth > 8:
        return 0
    total = len(re.findall(r"miscRng\.(?:random|randomBoolean)\(", text))
    for m in re.finditer(r"MonsterHelper\.(\w+)\(", text):
        n = m.group(1)
        if n in stack:
            continue
        h = helper_src(n)
        if h:
            total += real_misc_rolls(h, depth + 1, stack + (n,))
    return total


def engine_arms():
    src = open(f"{ROOT}/crates/core/src/run.rs").read()
    sec = src[src.index("fn resolve_encounter(&mut self"):]
    # 静态映射表（无 miscRng 消耗）
    static = {}
    m = re.search(r"fn static_encounter_kinds\(&self, name: &str\)[^{]*\{", src)
    if m:
        i = m.end() - 1
        depth, j = 1, i + 1
        while j < len(src) and depth:
            if src[j] == "{":
                depth += 1
            elif src[j] == "}":
                depth -= 1
            j += 1
        static = {"__body__": src[i:j]}
    out = {}
    for m in re.finditer(r'"([^"]+)"(?:\s*\|\s*"([^"]+)")?\s*=>\s*\{', sec):
        names = [m.group(1)] + ([m.group(2)] if m.group(2) else [])
        i = m.end() - 1
        depth, j = 1, i + 1
        while j < len(sec) and depth:
            if sec[j] == "{":
                depth += 1
            elif sec[j] == "}":
                depth -= 1
            j += 1
        for n in names:
            out[n] = sec[i:j]
    return out, static.get("__body__", "")


def main():
    real = real_encounters()
    eng, static_body = engine_arms()
    rows, bad = [], []
    for name in sorted(real):
        r = real_misc_rolls(real[name])
        body = eng.get(name)
        # 未在 resolve_encounter 里显式处理 → 落到 static_encounter_kinds（只查 act_pools，0 次掷）
        e = len(re.findall(r"self\.rng\.misc\.", body)) if body is not None else 0
        ok = e == r
        rows.append((name, r, e, ok))
        if not ok:
            bad.append((name, r, e))
    w = max(len(n) for n, *_ in rows)
    for name, r, e, ok in rows:
        print(f"{name:{w}s} 真={r:>2} 引擎={e:>2}  {'OK' if ok else '❌'}")
    print(f"\n审计 {len(rows)} 个遭遇：不一致 {len(bad)}")
    for b in bad:
        print("  ", b)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
