#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
从 CFR 反编译源码提取药水元数据表 → data/potions_full.json。

用法: python3 extract_potions.py <decompiled_dir> <out_dir>
产物字段: id / cls / rarity / isThrown / targetRequired / potency / zhName
定位: 数值真相源存档（引擎 potions.rs 与此表逐条对照；行为在 Rust 手写）。
"""
import json
import re
import sys
from datetime import date
from pathlib import Path

DEC = Path(sys.argv[1])
OUT = Path(sys.argv[2])

POT_DIR = DEC / "com" / "megacrit" / "cardcrawl" / "potions"


def method_body(text: str, sig: str) -> str | None:
    m = re.search(sig, text)
    if not m:
        return None
    i = text.find("{", m.start())
    depth = 0
    for j in range(i, len(text)):
        if text[j] == "{":
            depth += 1
        elif text[j] == "}":
            depth -= 1
            if depth == 0:
                return text[i + 1:j]
    return None


rows = []
for f in sorted(POT_DIR.glob("*.java")):
    if f.name == "AbstractPotion.java":
        continue
    text = f.read_text(encoding="utf-8", errors="ignore")
    pid = re.search(r'POTION_ID = "([^"]+)"', text)
    ctor = re.search(r"super\([^;]*PotionRarity\.([A-Z]+)[^;]*\)", text)
    pot_body = method_body(text, r"public int getPotency\(int ascensionLevel\)")
    pot = None
    if pot_body:
        m = re.search(r"return ([^;]+);", pot_body)
        if m:
            pot = m.group(1).strip()
    rows.append({
        "cls": f.stem,
        "id": pid.group(1) if pid else None,
        "rarity": ctor.group(1) if ctor else None,
        "isThrown": "this.isThrown = true" in text,
        "targetRequired": "this.targetRequired = true" in text,
        "potency": pot,  # 常量表达式（EntropicBrew = potionSlots 特例）
    })

# 中文名（zhs 本地化表，若存在）
zh = {}
zh_path = OUT / "zhs" / "potions.json"
if zh_path.exists():
    zh = json.loads(zh_path.read_text(encoding="utf-8"))
cls_to_zh = {}
for k, v in zh.items():
    if isinstance(v, dict) and "NAME" in v:
        cls_to_zh[k] = v["NAME"]

for r in rows:
    # zhs 表键 = POTION_ID（如 "Fire Potion"），少数 = 类名
    r["zhName"] = cls_to_zh.get(r["id"]) or cls_to_zh.get(r["cls"])

OUT.mkdir(parents=True, exist_ok=True)
out = OUT / "potions_full.json"
out.write_text(
    json.dumps({"date": str(date.today()), "count": len(rows), "data": rows},
               ensure_ascii=False, indent=1),
    encoding="utf-8")
print(f"potions: {len(rows)} → {out}")
