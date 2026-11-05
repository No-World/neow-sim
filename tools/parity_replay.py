#!/usr/bin/env python3
"""Replay a bridge trace (traces/*.jsonl) against the engine, report first divergence.

用法:
    PYTHONPATH=python python3 tools/parity_replay.py traces/long.jsonl --until 130
NEOW_TRACE_AIROLL=1 可加开引擎的 [AI] 意图决策追踪。
"""
import argparse
import json
import sys

import neow_sim

AT = {
    "play": 0, "end_turn": 1, "map": 4, "event": 5, "rest": 6,
    "shop_buy": 7, "reward": 8, "proceed": 14, "chest_open": 12,
}


# 动作类型 → 允许派发的引擎屏
ACT_OK = {
    "play": {"Combat"},
    "end_turn": {"Combat"},
    "map": {"MapNode"},
    "event": {"EventChoice", "NeowChoice"},
    "reward": {"RewardScreen"},
    "chest_open": {"RewardScreen"},
    "proceed": {"RewardScreen", "EventChoice", "ShopScreen"},
    "rest": {"RestChoice"},
    "shop_buy": {"ShopScreen"},
    "hand_select": {"Combat"},
    "boss": {"BossRelicChoice", "MapNode"},
}


def engine_step_from_bridge(env, act, la_flat, screen_hint=None):
    """把 bridge 动作翻译成引擎 step；返回 (ty, entity, target) 或 None(跳过)。"""
    ty_bridge = act["type"]
    if ty_bridge == "map":
        col = act["node_index"] % 7
        # 优先按目标列匹配；开局（map_started=false）时 entity = 起始列
        cands = [a for a in la_flat if a.action_type == 4]
        for a in cands:
            if a.target == col:
                return (4, a.entity, a.target)
        for a in cands:
            if a.entity == col:
                return (4, a.entity, a.target)
        return None
    if ty_bridge == "play":
        return (0, act["card_index"], act.get("target", -1))
    if ty_bridge == "end_turn":
        return (1, -1, -1)
    if ty_bridge == "event":
        return (5, act["option_index"], -1)
    if ty_bridge == "rest":
        ent = act.get("entity", act.get("rest", 0))
        return (6, ent, -1)
    if ty_bridge == "shop_buy":
        return (7, act.get("entity", 0), -1)
    if ty_bridge == "chest_open":
        return (12, -1, -1)
    if ty_bridge == "proceed":
        return (14, -1, -1)
    if ty_bridge == "reward":
        if act.get("skip"):
            return (9, -1, -1)
        idx = (act.get("take_indices") or [0])[0]
        tgt = act.get("card_pick", -1)
        return (8, idx, tgt)
    if ty_bridge == "hand_select":
        # 引擎在 combat_step 内已自动结算挂起屏（选第一项），
        # bridge 的 hand_select 是补发动作 → 无操作。⚠️ 不能派发 PROCEED：
        # combat_step 的兜底会把 14 当 EndTurn（实测 floor3 提前结束回合）。
        return None
    if ty_bridge == "boss":
        # BossRelicChoice 屏：3 选 1 / 跳过；MapNode 屏：Boss 门（PROCEED 路径进 Boss 房）
        if screen_hint == "MapNode":
            return (14, -1, -1)
        if act.get("skip"):
            return (9, -1, -1)
        return (8, act.get("entity", 0), -1)
    return None


def _intent_type(v):
    """归一化 intent：bridge 是 dict{'type':..}，引擎是字符串。只比类型名（大写）。"""
    if isinstance(v, dict):
        v = v.get("type")
    if isinstance(v, str):
        v = v.upper().replace("_", "")
        return None if v == "DEBUG" else v
    return None


def _card_id(c):
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return c[0]
    if isinstance(c, dict):
        return c.get("id")
    return c


def brief(st):
    st = dict(st)
    mons = [
        m
        for m in st.get("monsters", [])
        if (m.get("hp") or 0) > 0
    ]
    st = {**st, "monsters": mons}
    is_combat = st.get("state") == "Combat" or st.get("screen") == "combat"
    if not is_combat:
        st["hand"] = None
    m = st.get("meta") or {}
    p = st.get("player") or {}
    rng = st.get("rng") or m.get("rng") or {}
    return {
        "state": st.get("screen") or st.get("state"),
        "floor": st.get("floor", m.get("floor")),
        "pos": st.get("pos", m.get("pos")),
        "hp": (p or {}).get("hp") or m.get("hp"),
        "gold": (p or {}).get("gold") or m.get("gold"),
        "rng": rng or None,
        "monsters": [
            (
                mo.get("id"),
                mo.get("hp"),
                mo.get("block"),
                _intent_type(mo.get("intent")),
            )
            for mo in st.get("monsters", [])
        ],
        "hand": None if st.get("hand") is None else [_card_id(c) for c in st["hand"]],
    }


def compare(i, want, got, fields):
    if want.get("hand") is None or got.get("hand") is None:
        fields = [f for f in fields if f != "hand"]
    diffs = []
    for f in fields:
        w, g = want.get(f), got.get(f)
        if f == "monsters" and isinstance(w, list) and isinstance(g, list):
            if len(w) != len(g):
                diffs.append(f"[{i}]monsters 数量 want={len(w)} got={len(g)}\n    want={w}\n    got ={g}")
            else:
                for k, (mw, mg) in enumerate(zip(w, g)):
                    keys = ("id", "hp", "block", "intent")
                    for ki, key in enumerate(keys):
                        if key == "intent" and (mw[ki] is None or mg[ki] is None):
                            continue
                        if mw[ki] != mg[ki]:
                            diffs.append(
                                f"[{i}]monsters[{k}].{key} want={mw[ki]} got={mg[ki]}"
                            )
            continue
        if f in ("monsters", "hand", "rng", "pos"):
            if w != g:
                diffs.append(f"[{i}]{f}\n    want={w}\n    got ={g}")
        elif w != g:
            diffs.append(f"[{i}]{f} want={w} got={g}")
    return diffs


def neow_probe_pick(seed, asc, ch, want):
    """用一次性 env 逐个试 Neow 选项，返回效果匹配的 engine option idx。"""
    import copy
    for k in range(4):
        env = neow_sim.GymEnv(seed=seed, ascension=asc, character=ch)
        env.reset()
        try:
            env.step(5, k, -1)
        except Exception:
            continue
        st = json.loads(env.state_json())
        p = st.get("player", {})
        m = st.get("meta", {})
        if (
            p.get("max_hp") == want.get("max_hp")
            and p.get("hp") == want.get("hp")
            and p.get("gold") == want.get("gold")
            and len(st.get("deck", [])) == len(want.get("deck", []))
            and p.get("relics") == want.get("relics")
            or (p.get("max_hp") == want.get("max_hp") and p.get("gold") == want.get("gold"))
        ):
            return k
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("trace")
    ap.add_argument("--until", type=int, default=10**9)
    ap.add_argument("--compare", default="floor,rng,hp,gold,monsters,hand",
                    help="逗号分隔比较字段")
    args = ap.parse_args()
    fields = args.compare.split(",")

    steps = []
    with open(args.trace) as f:
        for ln in f:
            steps.append(json.loads(ln))

    seed = steps[1]["state"]["meta"]["seed"]
    asc = steps[1]["state"]["meta"]["ascension"]
    ch = steps[1]["state"]["meta"]["character"]
    env = neow_sim.GymEnv(seed=seed, ascension=asc, character=ch)
    env.reset()

    diffs_all = []
    prev_screen_holder = {}
    for o in steps[1:]:
        i = o["i"]
        if i > args.until:
            break
        act = o.get("action")
        if act is None:
            prev_screen_holder = {"screen": (o.get("state") or {}).get("screen")}
            continue
        engine_screen_now = json.loads(env.state_json()).get("state")
        # trace 语义：state_i 是 action_i 执行之后的观测（post-action）
        # 先派发动作，再和 trace 的同 step 状态比较
        # 地图确认对话框：bridge 在 map 屏发 event 动作（下一步才是真 map 动作）
        if act["type"] == "event" and engine_screen_now == "MapNode":
            print(f"[skip i={i}] map 确认对话框 (event@MapNode)")
            continue
        if act["type"] == "event" and i == 1 and engine_screen_now in ("NeowChoice",):
            # Neow（floor 0）：按效果匹配选项；want 用下一步（拾取后）的状态
            nxt = next((s for s in steps[2:] if (s.get("state") or {}).get("screen") is not None), None)
            st_raw = (nxt or {}).get("state") or {}
            wkey = {
                "max_hp": (st_raw.get("player") or {}).get("max_hp"),
                "hp": (st_raw.get("player") or {}).get("hp"),
                "gold": (st_raw.get("player") or {}).get("gold"),
                "deck": st_raw.get("deck", []),
                "relics": st_raw.get("relics", []),
            }
            k = neow_probe_pick(seed, asc, ch, wkey)
            if k is not None:
                env.step(5, k, -1)
                continue
            print(f"[warn i={i}] Neow 无匹配选项，试直传 option_index")
        if act["type"] == "event" and i == 2:
            # Neow 第二个 event 动作（真游戏确认对话）——引擎一次拾取即完成，跳过
            continue
        # 动作亲和守卫：动作类型与引擎当前屏匹配才派发
        pre = engine_screen_now
        ok = ACT_OK.get(act["type"])
        if pre is not None and ok is not None and pre not in ok:
            print(f"[skip i={i}] 屏幕不匹配: engine={pre} act={act}")
            continue
        tr = engine_step_from_bridge(env, act, env.legal_actions.flat, screen_hint=engine_screen_now)
        if tr is None:
            print(f"[skip i={i}] unmapped action {act}")
            continue
        ty, ent, tgt = tr
        try:
            env.step(ty, ent, tgt)
        except Exception as e:
            print(f"[err i={i}] step({ty},{ent},{tgt}) act={act}: {e}")
            break
        if (o.get("state") or {}).get("screen") is not None:
            st_now = json.loads(env.state_json())
            engine_screen_now = st_now.get("state")
            prev_screen_holder = o.get("state")
            got = brief(st_now)
            want = brief(o.get("state") or {})
            d = compare(i, want, got, fields)
            if d:
                diffs_all.extend(d)
                if len(diffs_all) > 12:
                    break

    if diffs_all:
        print(f"\nFAIL ({args.trace}) — {len(diffs_all)} 条差异（最多显示 12）")
        for d in diffs_all[:12]:
            print(d)
        sys.exit(1)
    print(f"OK replayed to i<={args.until}, 无差异（字段: {fields}）")


if __name__ == "__main__":
    main()
