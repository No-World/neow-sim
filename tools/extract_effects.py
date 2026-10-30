#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
全量效果提取：解析全部卡牌 use() 效果体 → 结构化 DSL（cards_full.json）。
产出含覆盖率统计：全解析 / 带分支 / 未支持（需手移植清单）。

用法: python3 extract_effects.py <decompiled_dir> <data_dir>
依赖: 同目录 extract_data.py 产出的 cards.json（数值表）
"""
import json
import re
import sys
from pathlib import Path

# 演出类动作（忽略，见 parity-spec 豁免表）
PERFORMATIVE = {
    "WaitAction", "VFXAction", "SFXAction", "SetAnimationAction", "AnimateHopAction",
    "AnimateSlowAttackAction", "AnimateFastAttackAction", "AnimateAttackAction",
    "ShakeScreenAction", "TextAboveCreatureAction", "RelicAboveCreatureAction",
    "TalkAction", "BlingAction", " SuicideAction",
}

POWER_IDS = {  # PowerXxx 类 → 协议 power id
    "StrengthPower": "Strength", "WeakPower": "Weakened", "VulnerablePower": "Vulnerable",
    "FrailPower": "Frail", "RitualPower": "Ritual", "ThornsPower": "Thorns",
    "MetallicizePower": "Metallicize", "RegenPower": "Regen", "PoisonPower": "Poison",
    "BarricadePower": "Barricade", "DexterityPower": "Dexterity", "ArtifactPower": "Artifact",
    "IntangiblePower": "Intangible", "BufferPower": "Buffer", "RagePower": "Rage",
    "CombustPower": "Combust", "DarkEmbracePower": "DarkEmbrace",
}


def balanced(s: str, start: int) -> tuple[str, int]:
    """s[start] 是 '('，返回 (括号内文本, 结束下标)"""
    depth = 0
    for j in range(start, len(s)):
        if s[j] == "(":
            depth += 1
        elif s[j] == ")":
            depth -= 1
            if depth == 0:
                return s[start + 1:j], j
    return s[start + 1:], len(s)


def split_top(args: str) -> list[str]:
    out, depth, cur = [], 0, []
    for ch in args:
        if ch == "(" or ch == "<":
            depth += 1
        elif ch == ")" or ch == ">":
            depth -= 1
        if ch == "," and depth == 0:
            out.append("".join(cur).strip())
            cur = []
        else:
            cur.append(ch)
    if cur:
        out.append("".join(cur).strip())
    return out


def expr(e: str):
    """参数表达式 → DSL"""
    e = e.strip().rstrip(";")
    m = {"this.magicNumber": "Magic", "this.baseMagicNumber": "Magic",
         "this.damage": "DamageBase", "this.block": "BlockBase",
         "p.currentBlock": "CurrentBlock", "this.energyOnUse": "XCost",
         "EnergyPanel.totalCount": "XCost"}
    if e in m:
        return m[e]
    if re.fullmatch(r"\d+", e):
        return int(e)
    if re.fullmatch(r"DMG|BLOCK|MAGIC|\w+", e):  # 常量名（提取表里已解析数值的地方不需要）
        return {"Const": e}
    return {"Raw": e[:60]}


def target_of(first_arg: str):
    a = first_arg.strip()
    if a in ("m", "(AbstractCreature)m", "mo", "(AbstractMonster)mo"):
        return "Enemy"
    if a in ("p", "(AbstractCreature)p", "player", "AbstractDungeon.player"):
        return "Self"
    return "Unknown"


def classify_action(cls: str, args: str):
    """一个动作构造 → Effect dict 或 None(忽略) 或 {'Unsupported': cls}"""
    parts = split_top(args)
    if cls == "DamageAction":
        di = re.search(r"new\s+DamageInfo\s*\(", args)
        if di:
            inner, _ = balanced(args, args.find("(", di.start()))
            dparts = split_top(inner)
            amount = expr(dparts[1]) if len(dparts) > 1 else "DamageBase"
        else:
            amount = "DamageBase"
        return {"Damage": [target_of(parts[0]) if parts else "Enemy", amount]}
    if cls == "DamageAllEnemiesAction":
        return {"DamageAll": "DamageBase"}
    if cls == "GainBlockAction":
        amount = expr(parts[-1]) if parts else "BlockBase"
        return {"Block": amount}
    if cls == "DrawCardAction":
        return {"Draw": expr(parts[-1]) if parts else 1}
    if cls == "ApplyPowerAction":
        pm = re.search(r"new\s+(\w+Power)\s*\(", args)
        power = POWER_IDS.get(pm.group(1), pm.group(1)) if pm else "?"
        pinner, _ = balanced(args, args.find("(", pm.start())) if pm else ("", 0)
        pparts = split_top(pinner)
        amount = expr(pparts[1]) if len(pparts) > 1 else 1
        tgt = target_of(parts[0]) if parts else "Self"
        return {"ApplyPower": [tgt, power, amount]}
    if cls == "GainEnergyAction":
        return {"GainEnergy": expr(parts[0]) if parts else 1}
    if cls == "HealAction":
        return {"Heal": expr(parts[-1]) if parts else 0}
    if cls == "MakeTempCardInDiscardAction":
        if "makeStatEquivalentCopy" in args:
            return {"CopySelfToDiscard": 1}
        cm = re.search(r"new\s+(\w+)\s*\(", args)
        n = expr(parts[1]) if len(parts) > 1 else 1
        return {"MakeInDiscard": [cm.group(1) if cm else "?", n]}
    if cls == "ExhaustAction" or cls == "ExhaustSpecificCardAction":
        return {"ExhaustHand": True}
    if cls in ("LoseHPAction",):
        return {"LoseHP": expr(parts[1]) if len(parts) > 1 else 0}
    if cls in PERFORMATIVE:
        return None
    return {"Unsupported": cls}


def strip_debug_branches(body: str) -> str:
    """剥离 if (Settings.isDebug) {...} 调试分支，保留 else 分支动作"""
    out = body
    while True:
        m = re.search(r"if\s*\(\s*Settings\.isDebug\s*\)\s*\{", out)
        if not m:
            return out
        start = out.find("{", m.start())
        depth = 0
        for j in range(start, len(out)):
            if out[j] == "{":
                depth += 1
            elif out[j] == "}":
                depth -= 1
                if depth == 0:
                    out = out[:m.start()] + " " + out[j + 1:]
                    break
        else:
            return out


def parse_use(raw_body: str):
    body = strip_debug_branches(raw_body)
    effects, unsupported = [], []
    i, has_branch = 0, bool(re.search(r"\bif\s*\(", body))
    while i < len(body):
        m = re.compile(r"new\s+([A-Z]\w+)\s*\(").search(body, i)
        if not m:
            break
        cls = m.group(1)
        inner, end = balanced(body, m.end() - 1)
        if cls.endswith("Action"):
            eff = classify_action(cls, inner)
            if eff is None:
                pass
            elif "Unsupported" in eff:
                unsupported.append(eff["Unsupported"])
                effects.append(eff)
            else:
                effects.append(eff)
        i = end + 1
    return effects, unsupported, has_branch


def method_body(text: str, sig: str):
    m = re.search(sig, text)
    if not m:
        return None
    start = text.find("{", m.start())
    depth = 0
    for j in range(start, len(text)):
        if text[j] == "{":
            depth += 1
        elif text[j] == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1:j]
    return None


def main():
    root = Path(sys.argv[1])
    data_dir = Path(sys.argv[2])
    cards = json.loads((data_dir / "cards.json").read_text(encoding="utf-8"))["data"]

    dirs = ["red", "green", "purple", "blue", "colorless", "curses", "tempCards", "status", "optionCards"]
    full, stats = [], {"parsed": 0, "parsed_branch": 0, "no_use": 0, "unsupported": 0}
    for card in cards:
        cls_file = card["class"]
        for d in dirs:
            f = root / "com/megacrit/cardcrawl/cards" / d / f"{cls_file}.java"
            if f.exists():
                break
        else:
            f = None
        entry = dict(card)
        entry["effects"] = []
        entry["status"] = "no_source"
        if f:
            text = f.read_text(errors="replace")
            body = method_body(text, r"public\s+void\s+use\s*\(")
            if body is None:
                entry["status"] = "no_use"  # 诅咒/状态等
                stats["no_use"] += 1
            else:
                effects, unsupported, branch = parse_use(body)
                entry["effects"] = effects
                if unsupported:
                    entry["status"] = "unsupported"
                    entry["unsupported"] = unsupported
                    stats["unsupported"] += 1
                elif branch:
                    entry["status"] = "parsed_branch"
                    stats["parsed_branch"] += 1
                else:
                    entry["status"] = "parsed"
                    stats["parsed"] += 1
        full.append(entry)

    meta = {"generator": "extract_effects.py", "note": "use() 效果 DSL；Unsupported 需手移植"}
    (data_dir / "cards_full.json").write_text(
        json.dumps({"meta": meta, "stats": stats, "data": full}, ensure_ascii=False, indent=1),
        encoding="utf-8")
    print(f"卡牌效果提取: {stats}")
    from collections import Counter
    uns = Counter()
    for e in full:
        for eff in e.get("effects", []):
            if isinstance(eff, dict) and "Unsupported" in eff:
                uns[eff["Unsupported"]] += 1
    print("未支持动作 TOP15:", uns.most_common(15))


if __name__ == "__main__":
    main()
