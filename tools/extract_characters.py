#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""提取 4 角色：起始 HP / 起始卡组 / 起始遗物（源：characters/*.java）"""
import json
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
out = Path(sys.argv[2]) / "characters.json"
res = []
for cls in ["Ironclad", "TheSilent", "Defect", "Watcher"]:
    t = (root / "com/megacrit/cardcrawl/characters" / f"{cls}.java").read_text(errors="replace")
    deck_m = re.search(r"getStartingDeck\s*\(\s*\)\s*\{(.*?)\n    \}", t, re.S)
    deck = re.findall(r'add\("([^"]+)"\)', deck_m.group(1)) if deck_m else []
    relic_m = re.search(r"getStartingRelics\s*\(\s*\)\s*\{(.*?)\n    \}", t, re.S)
    relics = re.findall(r'add\("([^"]+)"\)', relic_m.group(1)) if relic_m else []
    hp = None
    m = re.search(r"new\s+CharSelectInfo\(([^;]+)\);", t, re.S)
    if m:
        args = [a.strip() for a in m.group(1).split(",")]
        if len(args) > 3 and re.fullmatch(r"\d+", args[3]):
            hp = int(args[3])
    res.append({"id": cls, "max_hp": hp, "starting_deck": deck, "starting_relics": relics})
    print(f"{cls:9s} hp={hp} 卡组={len(deck)} 遗物={relics}")
out.write_text(json.dumps({"data": res}, ensure_ascii=False, indent=1), encoding="utf-8")
