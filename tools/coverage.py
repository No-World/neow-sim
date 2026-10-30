#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
覆盖率审计：数据层/行为层完整性 → docs/coverage.md。
用法: python3 coverage.py <data_dir> <repo_root>
"""
import json
import subprocess
import sys
from collections import Counter
from datetime import date
from pathlib import Path

DATA = Path(sys.argv[1])
ROOT = Path(sys.argv[2])

cards = json.loads((DATA / "cards_full.json").read_text(encoding="utf-8"))
stats = cards["stats"]
monsters = json.loads((DATA / "monsters.json").read_text(encoding="utf-8"))["data"]
powers = json.loads((DATA / "powers.json").read_text(encoding="utf-8"))["data"]
relics = json.loads((DATA / "relics.json").read_text(encoding="utf-8"))["data"]
potions = json.loads((DATA / "potions.json").read_text(encoding="utf-8"))["data"]
chars = json.loads((DATA / "characters.json").read_text(encoding="utf-8"))["data"]

# 引擎侧拒绝原因分布（与 cards.rs engine_playable 同逻辑）
ENGINE_POWERS = {"Strength", "Vulnerable", "Weakened", "Frail", "Ritual",
                 "LoseStrengthPower", "Dexterity", "Poison", "Metallicize", "Regeneration",
                 "Demon Form", "Berserk", "Brutality", "Barricade", "Thorns", "Artifact",
                 "IntangiblePlayer", "Energized", "Combust", "Dark Embrace", "Rage", "Focus"}


CLASS_NAMES = {c["class"] for c in cards["data"]}


def expr_ok(a):
    return not isinstance(a, dict)


def effect_supported(e):
    """与 cards.rs engine_playable 保持同步的镜像判定（递归支持 Repeat）"""
    k = next(iter(e))
    if k == "Damage":
        t, a = e[k]
        return t == "Enemy" and expr_ok(a)
    if k in ("DamageAll", "Block", "Draw", "GainEnergy", "Heal", "LoseHP"):
        return expr_ok(e[k])
    if k == "ApplyPower":
        _, p, a = e[k]
        return expr_ok(a) and p in ENGINE_POWERS
    if k == "CopySelfToDiscard":
        return True
    if k == "MakeInDiscard":
        cls, a = e[k]
        return expr_ok(a) and cls in CLASS_NAMES
    if k == "Repeat":
        cnt, inner = e[k]
        return expr_ok(cnt) and all(effect_supported(ie) for ie in inner)
    if k == "DamageRandom":
        return expr_ok(e[k])
    if k in ("MakeInHand", "MakeInDrawRandom", "MakeInDrawTop"):
        cls, a = e[k]
        return expr_ok(a) and cls in CLASS_NAMES
    if k == "CopySelfToHand":
        return expr_ok(e[k])
    if k in ("HandDiscard", "Scry"):
        return expr_ok(e[k][0])
    if k == "ChangeStance":
        return e[k] in ("Neutral", "Wrath", "Calm", "Divinity")
    if k == "Channel":
        orb, cnt, ev = e[k]
        return orb in ("Frost", "Lightning", "Dark", "Plasma") and expr_ok(cnt)
    if k in ("EvokeLeftmost", "EvokeNoRemove"):
        return True
    if k == "ShuffleDraw":
        return True
    if k == "OrbSlots":
        return expr_ok(e[k])
    return False  # ExhaustHand / Unsupported


def reject_reason(c):
    if c.get("cost") == -2:
        return "不可打出(-2费)"
    for e in c.get("effects", []):
        k = next(iter(e))
        if not effect_supported(e):
            if k == "Unsupported":
                return f"未支持动作: {e[k]}"
            if k == "Repeat":
                cnt, inner = e[k]
                bad = next((ie for ie in inner if not effect_supported(ie)), None)
                if bad is not None:
                    bk = next(iter(bad))
                    if bk == "Unsupported":
                        return f"未支持动作: {bad[bk]}"
                    if bk == "ApplyPower":
                        return f"power 未实现: {bad[bk][1]}"
                    return f"循环体内 {bk} 不支持"
                return "循环次数未解析"
            if k == "Damage":
                return f"伤害目标异常: {e[k][0]}"
            if k == "ApplyPower":
                return f"power 未实现: {e[k][1]}"
            if k == "MakeInDiscard":
                return "MakeInDiscard 类名映射"
            if k == "ExhaustHand":
                return "ExhaustAction 语义待核"
            return "表达式未解析"
    return None


reasons = Counter()
playable = 0
power_block = Counter()
for c in cards["data"]:
    r = reject_reason(c)
    if r is None:
        playable += 1
    else:
        reasons[r] += 1
        if r.startswith("power"):
            power_block[r.split(": ")[1]] += 1

unsup_actions = Counter()
for c in cards["data"]:
    for e in c.get("effects", []):
        if isinstance(e, dict) and "Unsupported" in e:
            unsup_actions[e["Unsupported"]] += 1

md = f"""# 覆盖率审计（自动生成：tools/coverage.py）

> 日期：{date.today()}。**完整 ≠ 无误**：完整 = 全系统可执行；无误 = L1/L2 对拍全绿（docs/parity-spec.md）。
> 本文件由脚本生成，随引擎推进重新执行更新。

## 漏斗总览（卡牌 {len(cards['data'])} 张）

| 层 | 数量 | 说明 |
|---|---|---|
| 数值表提取 | {len(cards['data'])}/375 | ✅ 全量 |
| use() 效果 DSL | {stats['parsed'] + stats['parsed_branch']}/375 | {stats['parsed']} 全解析 + {stats['parsed_branch']} 带分支 |
| 系统性未支持 | {sum(v for k, v in unsup_actions.items())} | 循环放招 {unsup_actions.get('loop', 0)}、充能球、姿态、Scry…（需系统级移植） |
| **引擎当前可执行** | **{playable}** | 数值+效果+power 白名单全通过 |

### 引擎拒绝原因分布（Top）

| 数量 | 原因 |
|---|---|
""" + "\n".join(f"| {v} | {k} |" for k, v in reasons.most_common(10)) + f"""

### 未实现 power 阻塞清单（实现即解锁，按解锁数排序）

| 卡数 | power |
|---|---|
""" + "\n".join(f"| {v} | {k} |" for k, v in power_block.most_common(12)) + f"""

## 其他实体层

| 内容 | 数据提取 | 行为实现 |
|---|---|---|
| 怪物 | {len(monsters)}/73 ✅ | 2/73（颚虫/邪教徒，手写行为） |
| power | {len(powers)} 条 ✅ | 11 种（Strength/易伤/虚弱/脆弱/Ritual/LoseStrength/敏捷/毒/金属化/再生） |
| 遗物 | {len(relics)} 条 ✅ | 0（战斗内主动遗物待做） |
| 药水 | {len(potions)} 条 ✅ | 0 |
| 角色 | {len(chars)}/4 ✅ | 1/4（铁甲，无角色专属机制） |

## 系统层

| 系统 | 状态 |
|---|---|
| 回合机/出牌管线/伤害格挡管线 | ✅ |
| 数值管线（calculateCardDamage 等 3 条，f32 逐位） | ✅ |
| RNG 13 流 + 洗牌管线（黄金向量对拍） | ✅ |
| 诚实型搜索 API（Clone/determinize/public_hash） | ✅ |
| 战斗内选择屏（hand/grid select、Scry） | ❌ |
| 充能球（Defect）/姿态（Watcher） | ❌ |
| 药水/主动遗物 | ❌ |
| 地图/事件/商店/休息/奖励（M3） | ❌ |
| pyo3 Python 绑定 | ❌ |
| **L1/L2 对拍（无误的最终裁判）** | ❌ 未开始（依赖 bridge） |
"""

out = ROOT / "docs" / "coverage.md"
out.write_text(md, encoding="utf-8")
print(f"引擎可执行卡牌: {playable}/375")
print(f"写入 {out}")
for k, v in reasons.most_common(6):
    print(f"  {v:4d}  {k}")
