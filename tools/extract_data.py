#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
从 CFR 反编译源码批量提取 STS 数值数据表。

用法: python3 extract_data.py <decompiled_dir> <out_dir>
产物: cards.json / monsters.json / relics.json / potions.json / powers.json

定位: 草稿表。静态文本挖掘，覆盖不到的留 raw/None 字段；
最终以 M4 真游戏回放对拍统一校验（docs/roadmap.md P2）。
"""
import json
import re
import sys
from datetime import date
from pathlib import Path

CARD_DIRS = {
    "red": "RED", "green": "GREEN", "purple": "PURPLE", "blue": "BLUE",
    "colorless": "COLORLESS", "curses": "CURSE", "tempCards": "TEMP", "status": "STATUS",
    "optionCards": "OPTION",
}


def method_body(text: str, sig: str) -> str | None:
    """按方法签名正则截取完整方法体（花括号配对）"""
    m = re.search(sig, text)
    if not m:
        return None
    i = text.find("{", m.start())
    depth, start = 0, i
    for j in range(i, len(text)):
        if text[j] == "{":
            depth += 1
        elif text[j] == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1:j]
    return None


def all_method_bodies(text: str, sig: str) -> list[str]:
    """同签名正则的全部方法体（重载构造器等）"""
    out = []
    for m in re.finditer(sig, text):
        i = text.find("{", m.start())
        depth, start = 0, i
        for j in range(i, len(text)):
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
                if depth == 0:
                    out.append(text[start + 1:j])
                    break
    return out


def static_ints(text: str) -> dict:
    out = {}
    for m in re.finditer(r"static\s+final\s+int\s+(\w+)\s*=\s*(-?\d+)\s*;", text):
        out[m.group(1)] = int(m.group(2))
    return out


def resolve(expr: str, consts: dict):
    """把表达式解析为 int：字面量 / 常量名 / const±const"""
    expr = expr.strip().replace("this.", "")
    if re.fullmatch(r"-?\d+", expr):
        return int(expr)
    m = re.fullmatch(r"(\w+)\s*([+-])\s*(\w+)", expr)
    if m:
        a = consts.get(m.group(1), int(m.group(1)) if m.group(1).lstrip("-").isdigit() else None)
        b = consts.get(m.group(3), int(m.group(3)) if m.group(3).lstrip("-").isdigit() else None)
        if a is not None and b is not None:
            return a + b if m.group(2) == "+" else a - b
    return None


def class_id(text: str, cls: str) -> str | None:
    m = re.search(r'static\s+final\s+String\s+ID\s*=\s*"([^"]+)"', text)
    if m:
        return m.group(1)
    m = re.search(r"super\(\s*\"([^\"]+)\"", text)
    return m.group(1) if m else cls


def _enum_name(arg: str):
    m = re.fullmatch(r"[\w.]+\.(\w+)", arg.strip())
    return m.group(1) if m else arg.strip()


def extract_cards(root: Path):
    cards = []
    base = root / "com/megacrit/cardcrawl/cards"
    for sub, color in CARD_DIRS.items():
        d = base / sub
        if not d.is_dir():
            continue
        for f in sorted(d.glob("*.java")):
            text = f.read_text(errors="replace")
            consts = static_ints(text)
            card = {"id": class_id(text, f.stem), "class": f.stem, "color": color,
                    "type": None, "rarity": None, "target": None, "cost": None,
                    "baseDamage": None, "baseBlock": None, "baseMagicNumber": None,
                    "flags": [], "review": []}
            # super(...) 九参：id, NAME, img, COST, DESCRIPTION, TYPE, COLOR, RARITY, TARGET
            ctor = method_body(text, r"public\s+" + f.stem + r"\s*\(")
            if ctor:
                sm = re.search(r"super\((.*?)\);", ctor, re.S)
                if sm:
                    sup = [a.strip() for a in sm.group(1).split(",")]
                    if len(sup) >= 9:
                        card["cost"] = resolve(sup[3], consts)
                        card["type"] = _enum_name(sup[5])
                        card["rarity"] = _enum_name(sup[7])
                        card["target"] = _enum_name(sup[8])
            for key, pat in [("type", r"this\.type\s*=\s*CardType\.(\w+)"),
                             ("rarity", r"this\.rarity\s*=\s*CardRarity\.(\w+)"),
                             ("target", r"this\.target\s*=\s*CardTarget\.(\w+)")]:
                mm = re.search(pat, text)
                if mm:
                    card[key] = mm.group(1)
            for key, pat in [("baseDamage", r"this\.baseDamage\s*=\s*([^;]+);"),
                             ("baseBlock", r"this\.baseBlock\s*=\s*([^;]+);"),
                             ("baseMagicNumber", r"this\.baseMagicNumber\s*=\s*([^;]+);")]:
                for mm in re.finditer(pat, text):
                    v = resolve(mm.group(1), consts)
                    if card[key] is None:
                        card[key] = v
                        if v is None:
                            card["review"].append(f"{key}={mm.group(1).strip()}")
            for flag, pat in [("exhaust", r"this\.exhaust\s*=\s*true"),
                              ("ethereal", r"this\.isEthereal\s*=\s*true"),
                              ("innate", r"this\.isInnate\s*=\s*true"),
                              ("selfRetain", r"this\.selfRetain\s*=\s*true"),
                              ("retain", r"this\.retain\s*=\s*true"),
                              ("purgeOnUse", r"this\.purgeOnUse\s*=\s*true")]:
                if re.search(pat, text):
                    card["flags"].append(flag)
            # 升级：upgrade() 方法体
            up = method_body(text, r"public\s+void\s+upgrade\s*\(\s*\)")
            card["upgrade"] = {}
            if up:
                for key, pat in [("damage", r"upgradeDamage\(([^)]+)\)"),
                                 ("block", r"upgradeBlock\(([^)]+)\)"),
                                 ("magicNumber", r"upgradeMagicNumber\(([^)]+)\)"),
                                 ("baseCost", r"upgradeBaseCost\(([^)]+)\)")]:
                    mm = re.search(pat, up)
                    if mm:
                        card["upgrade"][key] = resolve(mm.group(1), consts)
                upflags = []
                for flag, pat in [("exhaust", r"this\.exhaust\s*=\s*true"),
                                  ("innate", r"this\.isInnate\s*=\s*true"),
                                  ("selfRetain", r"this\.selfRetain\s*=\s*true"),
                                  ("ethereal", r"this\.isEthereal\s*=\s*true")]:
                    if re.search(pat, up):
                        upflags.append(flag)
                if upflags:
                    card["upgrade"]["flags"] = upflags
                if "upgradeName" not in up:
                    card["review"].append("no_upgradeName")
            else:
                card["review"].append("no_upgrade_method")  # 基础卡等
            cards.append(card)
    return cards


def extract_monsters(root: Path):
    monsters = []
    base = root / "com/megacrit/cardcrawl/monsters"
    files = sorted(base.glob("*.java")) + sorted(p for d in base.iterdir() if d.is_dir()
                                                 for p in d.glob("*.java"))
    for f in files:
        text = f.read_text(errors="replace")
        consts = static_ints(text)
        m = {"id": class_id(text, f.stem), "class": f.stem,
             "hp": [], "damage_infos": [], "asc_fields": {}, "moves": [],
             "ai_probabilities": [], "get_move_raw": None, "review": []}
        # HP：构造器里的 setHp 分支（含进阶阈值）——合并全部重载构造器
        src = "\n".join(all_method_bodies(text, r"(?:public|protected)\s+" + f.stem + r"\s*\([^)]*\)\s*\{"))
        for mm in re.finditer(r"ascensionLevel\s*>=\s*(\d+)\)?\s*\{([^{}]*)", src):
            asc, block = int(mm.group(1)), mm.group(2)
            if "setHp(" in block:
                h = re.search(r"setHp\(([^)]*)\)", block)
                lo, hi = _hp_pair(h.group(1), consts)
                m["hp"].append({"asc": asc, "min": lo, "max": hi})
            for fa in re.finditer(r"this\.(\w+)\s*=\s*(-?\d+)\s*;", block):
                m["asc_fields"].setdefault(str(asc), {})[fa.group(1)] = int(fa.group(2))
        h = re.search(r"(?<!\w)setHp\(([^)]*)\)", src)
        if h:
            lo, hi = _hp_pair(h.group(1), consts)
            if not m["hp"]:
                m["hp"].append({"asc": 0, "min": lo, "max": hi})
        # DamageInfo 序列（含 A 分档字段名则记 raw）
        for mm in re.finditer(r"this\.damage\.add\(\s*new\s+DamageInfo\(\s*[^,]+,\s*([^,)]+)", src):
            m["damage_infos"].append(resolve(mm.group(1), consts)
                                     if resolve(mm.group(1), consts) is not None
                                     else mm.group(1).strip())
        # setMove 调用（byte, intent, 可选 base）——按 (byte,intent) 去重
        seen = set()
        for mm in re.finditer(r"setMove\(\s*(?:[^,]*,)?\s*\(byte\)\s*(\d+)\s*,\s*"
                              r"(?:AbstractMonster\.)?Intent\.(\w+)\s*(?:,\s*(.*?))?\);",
                              text, re.S):
            key = (int(mm.group(1)), mm.group(2))
            if key in seen:
                continue
            seen.add(key)
            move = {"byte": key[0], "intent": key[1]}
            tail = (mm.group(3) or "").strip()
            if "damage.get" in tail:
                di = re.search(r"get\((\d+)\)", tail)
                if di:
                    move["damage_from_index"] = int(di.group(1))
            elif tail:
                v = resolve(tail, consts)
                move["base"] = v if v is not None else tail[:40]
            m["moves"].append(move)
        # takeTurn 的 case → 动作序列
        tt = method_body(text, r"public\s+void\s+takeTurn\s*\(\s*\)")
        if tt:
            acts = {}
            for cm in re.finditer(r"case\s+(\d+):\s*\{(.*?)break;", tt, re.S):
                seq = re.findall(r"new\s+(\w+Action)\(", cm.group(2))
                acts[int(cm.group(1))] = seq
            m["take_turn_actions"] = acts
        # 意图决策树原文 + 概率
        gm = method_body(text, r"protected\s+void\s+getMove\s*\(\s*int")
        if gm:
            m["get_move_raw"] = gm.strip()[:2000]
            m["ai_probabilities"] = [p for p in re.findall(r"randomBoolean\(([\d.]+)f?\)", gm)]
        monsters.append(m)
    return monsters


def _hp_pair(expr: str, consts: dict):
    parts = [p.strip() for p in expr.split(",")]
    if len(parts) == 1:
        v = resolve(parts[0], consts)
        return v, v
    return resolve(parts[0], consts), resolve(parts[1], consts)


def extract_simple(root: Path, pkg: str, cls_prefix: str, fields: dict):
    out = []
    d = root / "com/megacrit/cardcrawl" / pkg
    for f in sorted(d.glob("*.java")):
        if f.stem.startswith("Abstract"):
            continue
        text = f.read_text(errors="replace")
        consts = static_ints(text)
        e = {"id": class_id(text, f.stem), "class": f.stem}
        for key, pat in fields.items():
            mm = re.search(pat, text)
            if mm is None:
                e[key] = None
            elif mm.groups():
                e[key] = mm.group(1)
            else:
                e[key] = True  # 布尔标记模式（如 isThrown = true）
        out.append(e)
    return out


def main():
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".tools/decompiled-full")
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("neow-sim/data")
    out.mkdir(parents=True, exist_ok=True)

    cards = extract_cards(root)
    monsters = extract_monsters(root)
    relics = extract_simple(root, "relics", "AbstractRelic",
                            {"tier": r"this\.tier\s*=\s*RelicTier\.(\w+)",
                             "counter": r"this\.counter\s*=\s*(-?\d+)",
                             "price": r"this\.price\s*=\s*(\d+)"})
    potions = extract_simple(root, "potions", "AbstractPotion",
                             {"rarity": r"this\.rarity\s*=\s*PotionRarity\.(\w+)",
                              "isThrown": r"this\.isThrown\s*=\s*true",
                              "targetRequired": r"this\.targetRequired\s*=\s*true"})
    powers = extract_simple(root, "powers", "AbstractPower",
                            {"type": r"this\.type\s*=\s*PowerType\.(\w+)",
                             "isTurnBased": r"this\.isTurnBased\s*=\s*true"})

    meta = {"generator": "neow-sim/tools/extract_data.py",
            "source": "desktop-1.0.jar (CFR decompiled)", "date": str(date.today()),
            "note": "草稿表：文本挖掘产物，M4 对拍统一校验"}
    for name, data in [("cards", cards), ("monsters", monsters), ("relics", relics),
                       ("potions", potions), ("powers", powers)]:
        (out / f"{name}.json").write_text(
            json.dumps({"meta": meta, "data": data}, ensure_ascii=False, indent=1),
            encoding="utf-8")
        review = sum(1 for d in data if isinstance(d, dict) and d.get("review"))
        print(f"{name:10s} {len(data):4d} 条（待复核 {review}）")


if __name__ == "__main__":
    main()
