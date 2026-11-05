# -*- coding: utf-8 -*-
"""全流程对拍：bridge 轨迹 → 引擎，带**动作翻译层**。

难点：bridge 与引擎的屏幕状态机步数不同。最典型的是 Neow：
  bridge:  event(Talk) → event(选 1/4 祝福) → grid_select(选要变形的卡) → event(离开)
  引擎  :  单步 choose_neow_pick(idx)
所以不能 1:1 重放，必须按「屏幕 + 合法动作」翻译，并对不可比步显式 skip。

比对策略：
  - 只在两边都处于 **combat** 时逐字段比对（战斗是训练价值核心）
  - 非战斗屏只推进、不比对；结束时给出「战斗步数 / 通过步数」统计
  - 首个分歧给出完整上下文（第几步、哪个字段、期望/实际）

用法:
  python3 tests/conformance/parity_run.py traces/pmcwkzef.jsonl
  python3 tests/conformance/parity_run.py traces/*.jsonl --fields player,hand,monsters
"""
import argparse
import json
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _here)
# 必须把 neow-sim/python 放到最前面：否则 site-packages 里的旧 wheel 会抢先被导入
sys.path.insert(0, os.path.normpath(os.path.join(_here, "..", "..", "python")))

import neow_sim                      # noqa: E402
from parity_diff import (            # noqa: E402
    BRIDGE_MAP,
    engine_snapshot,
    extract_entity,
    extract_target,
    normalize,
    diff_state,
)

MAP_WIDTH = 7


def engine_screen(env):
    d = json.loads(env.state_json())
    return d.get("state", "")


def engine_floor(env):
    return json.loads(env.state_json())["meta"]["floor"]


def bridge_steps(rows):
    """把轨迹变成 (before_screen, action, after_state, i) 序列。"""
    out = []
    prev_screen = None
    for r in rows:
        if r.get("action") is None:
            prev_screen = (normalize(r.get("state")) or {}).get("screen")
            continue
        out.append((prev_screen, r["action"], r.get("state"), r.get("i")))
        prev_screen = (normalize(r.get("state")) or {}).get("screen")
    return out


def resolve_map_column(env, node_index):
    """bridge 的 node_index → 引擎的 (模式, 值)。

    bridge MapObs 的节点序号是绝对下标（y*WIDTH + x）；引擎需要
      - 未起步：row0 的列
      - 已起步：当前节点的出边下标
    """
    st = json.loads(env.state_json())
    return st, node_index


def run_trace(path, fields, verbose=False, max_report=25):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    if not rows:
        return {"path": path, "error": "empty"}

    seed = None
    asc = 0
    for r in rows:
        st = normalize(r.get("state")) or {}
        meta = (r.get("state") or {}).get("meta") or {}
        if meta.get("seed"):
            seed = meta["seed"]
            asc = meta.get("ascension", 0) or 0
        if seed:
            break
    if seed is None:
        seed = 42
    env = neow_sim.GymEnv(seed=int(seed), ascension=int(asc), character="IRONCLAD")
    env.reset()

    stats = {"steps": 0, "combat_steps": 0, "compared": 0, "skipped": 0, "combat_end": 0,
             "unmapped": [], "diffs": [], "first_divergence": None}
    eng_screen = engine_screen(env)

    for before, act, after_raw, i in bridge_steps(rows):
        atype = act.get("type", "")
        ent = extract_entity(act)
        tgt = extract_target(act)

        # ── 翻译 ──────────────────────────────────────────────
        ename = eng_screen
        if ename == "NeowChoice":
            # bridge 的 Neow 是 4 步；引擎 1 步。
            # 只在「祝福选择」那一步真正推进引擎（合法动作里 option_index 有 2..4 个）
            if atype == "event" and before == "event":
                # Talk 屏通常只有 1 个选项 → 引擎不需要动
                n_opts = _neow_opt_count(env)
                if n_opts > 1 and 0 <= ent < n_opts:
                    env.step(5, ent, -1)      # CHOOSE_EVENT
                else:
                    stats["skipped"] += 1
            else:
                stats["skipped"] += 1         # grid_select / 最后的 leave
            eng_screen = engine_screen(env)
            continue

        if ename == "MapNode":
            if atype != "map":
                stats["skipped"] += 1
                eng_screen = engine_screen(env)
                continue
            col = int(ent) % MAP_WIDTH if not str(ent).startswith("-") else 0
            if os.environ.get("NEOW_TRACE_MAP"):
                st = json.loads(env.state_json())
                print("  [MAP] i=%s node_index=%s -> col=%s engine_pos=%s floor=%s" %
                      (i, ent, col, st["meta"].get("pos"), st["meta"]["floor"]))
            ok = _choose_map(env, col)
            if not ok:
                stats["unmapped"].append((i, "map col=%d" % col))
            eng_screen = engine_screen(env)
            continue

        if ename == "Combat":
            if os.environ.get("NEOW_TRACE_CBT") and atype == "play":
                st = json.loads(env.state_json())
                h = [c["id"] for c in (st.get("hand") or [])]
                print("  [CBT] i=%s ent=%s bridge_card=%s engine_hand=%s" %
                      (i, ent, act.get("card_id"), h))
            if atype not in ("play", "end_turn", "potion", "potion_discard"):
                stats["skipped"] += 1
                continue
            ty = BRIDGE_MAP.get(atype)
            if ty is None:
                stats["skipped"] += 1
                continue
            try:
                env.step(ty, ent, tgt)
            except Exception as e:
                stats["diffs"].append((i, "step", "ok", "raise: %s" % e))
                stats["first_divergence"] = (i, "step", "ok", str(e))
                break
            stats["steps"] += 1
            stats["combat_steps"] += 1

            want = normalize(after_raw)
            if not want:
                continue
            # 只比「双方都还在战斗」的步。战斗刚结束那一步（bridge 进 combat_reward）
            # 比的是战后记账（手牌是否清空、奖励何时入账），与战斗规则无关。
            if want.get("screen") != "combat":
                stats["combat_end"] = stats.get("combat_end", 0) + 1
                eng_screen = engine_screen(env)
                continue
            got = engine_snapshot(env)
            d = diff_state(want, got, fields, prefix="[%d]" % i)
            if d:
                stats["diffs"].extend(d)
                if stats["first_divergence"] is None:
                    stats["first_divergence"] = d[0]
                if len(stats["diffs"]) >= max_report:
                    break
            else:
                stats["compared"] += 1
            eng_screen = engine_screen(env)
            continue

        # ── 奖励屏：bridge 在 combat_reward 时引擎可能还停在 Combat/MapNode，
        # 必须先把引擎推进到 RewardScreen，否则 TAKE_REWARD 会被跳过（deck 少卡）。
        if atype in ("reward", "skip", "proceed") and ename != "RewardScreen":
            # 引擎若在 Combat，先让它自然结束（end_turn）
            guard = 0
            while ename == "Combat" and guard < 5:
                try:
                    env.step(1, -1, -1)   # END_TURN
                except Exception:
                    break
                ename = engine_screen(env)
                guard += 1
            # 引擎若在 MapNode，进当前节点的房间（choose_node 0）
            if ename == "MapNode":
                la = env.legal_actions.flat
                if la:
                    env.step(la[0].action_type, la[0].entity, la[0].target)
                    ename = engine_screen(env)

        # 其它屏（reward/rest/shop/event…）：尝试用通用映射推进，不比对
        if os.environ.get("NEOW_TRACE_REW"):
            st = json.loads(env.state_json())
            print("  [REW] i=%s bridge=%s engine=%s act=%s ent=%s pos=%s floor=%s" %
                  (i, before, ename, atype, ent, st["meta"].get("pos"), st["meta"]["floor"]))
        ty = BRIDGE_MAP.get(atype)
        if ty is not None:
            try:
                env.step(ty, ent, tgt)
                stats["steps"] += 1
            except Exception as e:
                if os.environ.get("NEOW_TRACE_REW"):
                    print("  [REW] SKIP i=%s engine=%s act=%s ent=%s err=%s" % (i, ename, atype, ent, e))
                stats["skipped"] += 1
        else:
            if os.environ.get("NEOW_TRACE_REW"):
                print("  [REW] UNMAPPED i=%s act=%s" % (i, atype))
            stats["skipped"] += 1
        eng_screen = engine_screen(env)

    stats["ok"] = not stats["diffs"]
    stats["path"] = path
    return stats


def _neow_opt_count(env):
    try:
        return len(env.neow_options)
    except Exception:
        return 0


def _choose_map(env, col):
    """让引擎走到 row0 的目标列 / 或当前节点指向该列的边。

    ⚠️ 必须先判「已起步」：已起步时 entity 是**边下标**、target 才是目标列。
    若先按「entity == col」匹配，会把「边下标 1」误当成「列 1」
    （实测 node(8,2) edges=[1,2,3]，col=1 时错选了 target=2 的边，进了 Event 而非 Monster）。
    """
    st = json.loads(env.state_json())
    started = bool(st["meta"].get("pos")) and st["meta"]["floor"] > 0
    la = env.legal_actions
    acts = [(a.action_type, a.entity, a.target) for a in la.flat]
    if started:
        # 已起步：优先按 target 匹配目标列
        for ty, e, t in acts:
            if ty == 4 and t == col:
                env.step(4, e, -1)
                return True
        for ty, e, t in acts:
            if ty == 4 and t >= 0 and t != col:
                continue
    else:
        # 首次选列：entity 就是列号
        for ty, e, t in acts:
            if ty == 4 and e == col:
                env.step(4, e, -1)
                return True
    # 兜底走第一条
    for ty, e, t in acts:
        if ty == 4:
            env.step(4, e, -1)
            return False
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("traces", nargs="+")
    ap.add_argument("--fields", default="player,hand,monsters,piles")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()
    fields = set(x.strip() for x in args.fields.split(",") if x.strip())

    total_ok = total_fail = 0
    for path in args.traces:
        r = run_trace(path, fields, args.verbose)
        if r.get("error"):
            print("%-32s ERROR %s" % (os.path.basename(path), r["error"]))
            total_fail += 1
            continue
        tag = "PASS" if r["ok"] else "FAIL"
        print("%-32s %s  combat=%d end=%d compared=%d skipped=%d unmapped=%d diffs=%d" % (
            os.path.basename(path), tag, r["combat_steps"], r.get("combat_end", 0),
            r["compared"], r["skipped"], len(r["unmapped"]), len(r["diffs"])))
        if r["diffs"]:
            for item in r["diffs"][:15]:
                print("    %-26s want=%r got=%r" % (item[0], item[1], item[2]))
            total_fail += 1
        else:
            total_ok += 1
        if r["unmapped"] and args.verbose:
            print("    unmapped:", r["unmapped"][:5])

    print("\n=== %d PASS / %d FAIL ===" % (total_ok, total_fail))
    return 0 if total_fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
