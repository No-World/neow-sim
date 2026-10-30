#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
全量效果提取：解析全部卡牌 use() 效果体 → 结构化 DSL（cards_full.json）。
- 解析类常量（DMG 等 static final int）
- 解析 use() 内的动态赋值（Body Slam: baseDamage = p.currentBlock）
- 剥离 Settings.isDebug 调试分支；循环放招显式标记 unsupported(loop)

用法: python3 extract_effects.py <decompiled_dir> <data_dir>
依赖: extract_data.py 产出的 cards.json（数值表）
"""
import json
import re
import sys
from pathlib import Path

PERFORMATIVE = {
    "WaitAction", "VFXAction", "SFXAction", "SetAnimationAction", "AnimateHopAction",
    "AnimateSlowAttackAction", "AnimateFastAttackAction", "AnimateAttackAction",
    "ShakeScreenAction", "TextAboveCreatureAction", "RelicAboveCreatureAction",
    "TalkAction", "BlingAction",
}

POWER_IDS = {
    "StrengthPower": "Strength", "WeakPower": "Weakened", "VulnerablePower": "Vulnerable",
    "FrailPower": "Frail", "RitualPower": "Ritual", "ThornsPower": "Thorns",
    "MetallicizePower": "Metallicize", "RegenPower": "Regeneration", "PoisonPower": "Poison",
    "BarricadePower": "Barricade", "DexterityPower": "Dexterity", "ArtifactPower": "Artifact",
    "IntangiblePower": "Intangible", "BufferPower": "Buffer", "RagePower": "Rage",
    "CombustPower": "Combust", "DarkEmbracePower": "DarkEmbrace",
    "LoseStrengthPower": "LoseStrengthPower", "GainStrengthPower": "GainStrengthPower",
    "CurlUpPower": "CurlUp", "SporeCloudPower": "SporeCloud",
}


def balanced(s: str, start: int) -> tuple[str, int]:
    """通用括号配对：s[start] 是 '(' 或 '{'，返回（内部文本, 结束下标）"""
    op, cl = ("(", ")") if s[start] == "(" else ("{", "}")
    depth = 0
    for j in range(start, len(s)):
        if s[j] == op:
            depth += 1
        elif s[j] == cl:
            depth -= 1
            if depth == 0:
                return s[start + 1:j], j
    return s[start + 1:], len(s)


def split_top(args: str) -> list[str]:
    out, depth, cur = [], 0, []
    for ch in args:
        if ch in "(<":
            depth += 1
        elif ch in ")>":
            depth -= 1
        if ch == "," and depth == 0:
            out.append("".join(cur).strip())
            cur = []
        else:
            cur.append(ch)
    if cur:
        out.append("".join(cur).strip())
    return out


def static_ints(text: str) -> dict:
    out = {}
    for m in re.finditer(r"static\s+final\s+int\s+(\w+)\s*=\s*(-?\d+)\s*;", text):
        out[m.group(1)] = int(m.group(2))
    return out


FIELD_OF = {"this.damage": "baseDamage", "this.block": "baseBlock",
            "this.magicNumber": "baseMagicNumber"}


def expr(e: str, consts: dict, overrides: dict, lvars: dict | None = None):
    """参数表达式 → DSL（consts=类常量，overrides=use() 内动态赋值）"""
    e = e.strip().rstrip(";")
    if e in FIELD_OF and FIELD_OF[e] in overrides:
        return overrides[FIELD_OF[e]]
    m = {"this.magicNumber": "Magic", "this.baseMagicNumber": "Magic",
         "this.damage": "DamageBase", "this.block": "BlockBase",
         "p.currentBlock": "CurrentBlock", "this.energyOnUse": "XCost",
         "EnergyPanel.totalCount": "XCost"}
    if e in m:
        return m[e]
    if re.fullmatch(r"-?\d+", e):
        return int(e)
    if lvars and re.fullmatch(r"\w+", e) and e in lvars:
        return lvars[e]
    if consts and re.fullmatch(r"\w+", e) and e in consts:
        return consts[e]
    if re.fullmatch(r"\w+", e):
        return {"Const": e}
    return {"Raw": e[:60]}


def target_of(first_arg: str):
    a = first_arg.strip()
    if a in ("m", "(AbstractCreature)m", "mo", "(AbstractMonster)mo"):
        return "Enemy"
    if a in ("p", "(AbstractCreature)p", "player", "AbstractDungeon.player"):
        return "Self"
    return "Unknown"


NUMERICISH = re.compile(r"^-?\d+$|magicNumber|this\.damage|this\.block|currentBlock|energyOnUse")


def classify_action(cls: str, args: str, consts: dict, overrides: dict, lvars: dict | None = None):
    parts = split_top(args)
    if cls == "DamageAction":
        di = re.search(r"new\s+DamageInfo\s*\(", args)
        if di:
            inner, _ = balanced(args, args.find("(", di.start()))
            dparts = split_top(inner)
            amount = expr(dparts[1], consts, overrides) if len(dparts) > 1 else "DamageBase"
        else:
            amount = "DamageBase"
        return {"Damage": [target_of(parts[0]) if parts else "Enemy", amount]}
    if cls == "DamageAllEnemiesAction":
        return {"DamageAll": "DamageBase"}
    if cls == "GainBlockAction":
        return {"Block": expr(parts[-1], consts, overrides) if parts else "BlockBase"}
    if cls == "DrawCardAction":
        return {"Draw": expr(parts[-1], consts, overrides) if parts else 1}
    if cls == "ApplyPowerAction":
        pm = re.search(r"new\s+(\w+Power)\s*\(", args)
        power = POWER_IDS.get(pm.group(1), pm.group(1)) if pm else "?"
        pinner, _ = balanced(args, args.find("(", pm.start())) if pm else ("", 0)
        pparts = split_top(pinner)
        # 取最后一个数值型参数（兼容 (owner, amount) 与 (owner, source, amount) 构造）
        amount = None
        for cand in reversed(pparts[1:] if len(pparts) > 1 else []):
            if NUMERICISH.search(cand):
                amount = expr(cand, consts, overrides)
                break
        if amount is None:
            amount = expr(pparts[1], consts, overrides) if len(pparts) > 1 else 1
        tgt = target_of(parts[0]) if parts else "Self"
        return {"ApplyPower": [tgt, power, amount]}
    if cls == "GainEnergyAction":
        return {"GainEnergy": expr(parts[0], consts, overrides) if parts else 1}
    if cls == "HealAction":
        return {"Heal": expr(parts[-1], consts, overrides) if parts else 0}
    if cls == "MakeTempCardInDiscardAction":
        if "makeStatEquivalentCopy" in args:
            return {"CopySelfToDiscard": 1}
        cm = re.search(r"new\s+(\w+)\s*\(", args)
        n = expr(parts[1], consts, overrides) if len(parts) > 1 else 1
        return {"MakeInDiscard": [cm.group(1) if cm else "?", n]}
    if cls in ("ExhaustAction", "ExhaustSpecificCardAction"):
        return {"ExhaustHand": True}
    if cls == "DiscardAction":
        # (target, source, amount, isRandom[, endTurn])
        amount = expr(parts[2], consts, overrides, lvars) if len(parts) > 2 else 1
        is_random = len(parts) > 3 and parts[3].strip() == "true"
        return {"HandDiscard": [amount, is_random]}
    if cls == "ScryAction":
        # ScryAction(int numCards)：占卜 N（GoldenEye 遗物加成 M3 接）
        return {"Scry": expr(parts[0], consts, overrides, lvars) if parts else 1}
    if cls == "MakeTempCardInHandAction":
        if "makeStatEquivalentCopy" in args or "makeSameInstanceOf" in args:
            return {"CopySelfToHand": expr(parts[1], consts, overrides, lvars) if len(parts) > 1 else 1}
        cm = re.search(r"new\s+(\w+)\s*\(", args)
        n = expr(parts[1], consts, overrides, lvars) if len(parts) > 1 else 1
        return {"MakeInHand": [cm.group(1) if cm else "?", n]}
    if cls == "MakeTempCardInDrawPileAction":
        cm = re.search(r"new\s+(\w+)\s*\(", args)
        n = expr(parts[1], consts, overrides, lvars) if len(parts) > 1 else 1
        random_spot = len(parts) > 2 and parts[2].strip() == "true"
        kind = "MakeInDrawRandom" if random_spot else "MakeInDrawTop"
        return {kind: [cm.group(1) if cm else "?", n]}
    if cls == "AttackDamageRandomEnemyAction":
        # 目标 = getRandomMonster(null, true, cardRandomRng)——引擎用 card_random 流复刻
        return {"DamageRandom": "DamageBase"}
    if cls == "PummelDamageAction":
        # (target, info[, times])：同目标多次伤害
        if len(parts) >= 3:
            times = expr(parts[2], consts, overrides, lvars)
            return {"Repeat": [times, [{"Damage": ["Enemy", "DamageBase"]}]]}
        return {"Damage": ["Enemy", "DamageBase"]}
    if cls == "LoseHPAction":
        return {"LoseHP": expr(parts[1], consts, overrides) if len(parts) > 1 else 0}
    if cls in PERFORMATIVE:
        return None
    return {"Unsupported": cls}


def strip_debug_branches(body: str) -> str:
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


def collect_overrides(body: str, consts: dict) -> dict:
    """use() 内对 this.baseDamage/this.baseBlock/this.baseMagicNumber 的动态赋值（Body Slam 型）
    ——赋值后卡内再读 this.damage 即用该值（calculateCardDamage 的输入）"""
    overrides = {}
    for field, key in {
        "baseDamage": "baseDamage",
        "baseBlock": "baseBlock",
        "baseMagicNumber": "baseMagicNumber",
    }.items():
        m = re.search(r"this\." + field + r"\s*=\s*([^;,)]+)[;,]", body)
        if m:
            v = expr(m.group(1), consts, {})
            if isinstance(v, (int, str)):  # 数值或 CurrentBlock 等 DSL 名
                overrides[key] = v
    return overrides


def collect_lvars(body: str) -> dict:
    """局部计数变量：`int x = 3` / `x = this.energyOnUse`（X 费次数）"""
    lv = {}
    for m in re.finditer(r"\bint\s+(\w+)\s*=\s*(-?\d+)\s*;", body):
        lv[m.group(1)] = int(m.group(2))
    for m in re.finditer(r"\b(\w+)\s*=\s*this\.energyOnUse\s*;", body):
        lv[m.group(1)] = "XCost"
    for m in re.finditer(r"\bint\s+(\w+)\s*=\s*this\.(magicNumber|baseMagicNumber)\s*;", body):
        lv[m.group(1)] = "Magic"
    for m in re.finditer(r"\bint\s+(\w+)\s*=\s*(this\.damage|this\.block)\s*;", body):
        lv[m.group(1)] = "DamageBase" if "damage" in m.group(2) else "BlockBase"
    return lv


def resolve_bound(bound: str, consts: dict, overrides: dict, lvars: dict):
    v = expr(bound, consts, overrides)
    if isinstance(v, (int, str)):
        return v
    if bound.strip() in lvars:
        return lvars[bound.strip()]
    return {"Raw": bound[:60]}


FOR_RE = re.compile(r"\bfor\s*\(")
NEW_RE = re.compile(r"new\s+([A-Z]\w+)\s*\(")


def scan_actions(text: str, consts: dict, overrides: dict, lvars: dict, depth: int):
    """按源码顺序扫描：for 循环 → Repeat DSL；动作构造 → 效果 DSL"""
    effects, unsupported = [], []
    i = 0
    while i < len(text):
        mf = FOR_RE.search(text, i)
        mn = NEW_RE.search(text, i)
        if not mf and not mn:
            break
        if mf and (not mn or mf.start() < mn.start()):
            if depth >= 3:
                return [{"Unsupported": "nested-loop"}], ["nested-loop"]
            hdr_open = text.find("(", mf.start())
            hdr, close = balanced(text, hdr_open)
            bm = re.search(r"<\s*([^;]+)\s*;", hdr)
            cnt = resolve_bound(bm.group(1) if bm else "1", consts, overrides, lvars)
            j = close + 1
            while j < len(text) and text[j] in " \t\r\n":
                j += 1
            if j < len(text) and text[j] == "{":
                inner, end = balanced(text, j)
            else:
                semi = text.find(";", j)
                inner, end = text[j:max(semi, j)], max(semi - 1, j)
            inner_effects, inner_unsup = scan_actions(
                inner, consts, overrides, lvars, depth + 1)
            effects.append({"Repeat": [cnt, inner_effects]})
            unsupported.extend(inner_unsup)
            i = end + 1
        else:
            cls = mn.group(1)
            args, end = balanced(text, mn.end() - 1)
            if cls.endswith("Action"):
                eff = classify_action(cls, args, consts, overrides, lvars)
                if eff is not None:
                    effects.append(eff)
                    if "Unsupported" in eff:
                        unsupported.append(eff["Unsupported"])
            i = end + 1
    return effects, unsupported


def parse_use(raw_body: str, consts: dict):
    body = strip_debug_branches(raw_body)
    overrides = collect_overrides(body, consts)
    lvars = collect_lvars(body)
    if re.search(r"\bwhile\s*\(", body):
        return [{"Unsupported": "while-loop"}], ["while-loop"], False
    effects, unsupported = scan_actions(body, consts, overrides, lvars, 0)
    has_branch = bool(re.search(r"\bif\s*\(", body))
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


def follow_action(root: Path, actions_index: dict, cls: str, call_args: str,
                  consts: dict, overrides: dict, lvars: dict):
    """跟进自定义动作类：解析其 update() 体，this.amount 映射回调用点参数，
    this.multiDamage/this.damage 映射为 DamageBase。仅在完全可解析时返回效果列表。"""
    f = actions_index.get(cls)
    if f is None:
        return None
    text = f.read_text(errors="replace")
    up = method_body(text, r"public\s+void\s+update\s*\(")
    if up is None:
        return None
    # 找到动作构造器中名为 amount 的参数位置
    pos = None
    for cm in re.finditer(r"public\s+" + cls + r"\s*\(([^)]*)\)", text):
        params = [a.strip() for a in cm.group(1).split(",")]
        for i, prm in enumerate(params):
            if prm.endswith(" amount") or prm.endswith(" int amount"):
                pos = i
        if pos is not None:
            break
    count_val = "XCost"
    if pos is not None:
        args = split_top(call_args)
        if pos < len(args):
            count_val = expr(args[pos], consts, overrides)
            if isinstance(count_val, dict):
                count_val = lvars.get(args[pos].strip(), count_val)
    # 动作体内变量替换表
    sub = {"this.amount": count_val if isinstance(count_val, (int, str)) else 1,
           "this.multiDamage": "DamageBase", "this.damage": "DamageBase",
           "this.block": "BlockBase", "this.magicNumber": "Magic"}
    ov2 = dict(overrides)
    for k, v in sub.items():
        ov2[k] = v
    # expr() 识别的字段名不带 this. 前缀——改写映射键
    ov3 = {}
    for k, v in ov2.items():
        ov3[k] = v
    effs, unsup = scan_actions(up, consts, ov3, collect_lvars(up), 1)
    # 洁净检查：拒绝空效果（静默 no-op）、空循环体、未解析次数——只接收完整可执行的动作体
    if unsup or not effs or not effects_clean(effs):
        return None
    return effs


def effects_clean(effs) -> bool:
    for e in effs:
        k = next(iter(e))
        v = e[k]
        if k == "Repeat":
            cnt, inner = v
            if isinstance(cnt, dict) or not inner:
                return False
            if not effects_clean(inner):
                return False
        elif isinstance(v, list):
            for item in v:
                if isinstance(item, dict):
                    return False
                if isinstance(item, list):
                    for x in item:
                        if isinstance(x, dict):
                            return False
        elif isinstance(v, dict):
            return False
    return True


def main():
    root = Path(sys.argv[1])
    data_dir = Path(sys.argv[2])
    cards = json.loads((data_dir / "cards.json").read_text(encoding="utf-8"))["data"]
    actions_index = {q.stem: q for q in (root / "com/megacrit/cardcrawl/actions").rglob("*.java")}

    dirs = ["red", "green", "purple", "blue", "colorless", "curses", "tempCards", "status", "optionCards"]
    full, stats = [], {"parsed": 0, "parsed_branch": 0, "no_use": 0, "unsupported": 0}
    for card in cards:
        f = None
        for d in dirs:
            cand = root / "com/megacrit/cardcrawl/cards" / d / f"{card['class']}.java"
            if cand.exists():
                f = cand
                break
        entry = dict(card)
        entry["effects"] = []
        entry["status"] = "no_source"
        if f:
            text = f.read_text(errors="replace")
            body = method_body(text, r"public\s+void\s+use\s*\(")
            if body is None:
                entry["status"] = "no_use"
                stats["no_use"] += 1
            else:
                consts = static_ints(text)
                effects, unsupported, branch = parse_use(body, consts)
                # 自定义动作跟进：尝试把 Unsupported(ActionCls) 换成动作体效果
                for ei, eff in enumerate(list(effects)):
                    if isinstance(eff, dict) and "Unsupported" in eff:
                        cls = eff["Unsupported"]
                        cm = re.search(r"new\s+" + cls + r"\s*\(", body)
                        call_args = ""
                        if cm:
                            call_args, _ = balanced(body, cm.end() - 1)
                        lv = collect_lvars(body)
                        spliced = follow_action(root, actions_index, cls, call_args,
                                                consts, collect_overrides(body, consts), lv)
                        if spliced:
                            effects[ei:ei + 1] = spliced
                            unsupported = [u for u in unsupported if u != cls]
                effects, unsupported = effects, unsupported
                if not unsupported and branch:
                    pass
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
