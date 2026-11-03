# -*- coding: utf-8 -*-
"""M4 对拍：真游戏 golden trace ←→ neow-sim，逐字段 diff。

与旧 parity_check.py 的区别：
  旧版只重放动作、断言「不抛异常」；本版真正比对状态快照，任何字段不符即 FAIL。

用法:
  python3 parity_diff.py traces/pmcwkzef.jsonl
  python3 parity_diff.py traces/*.jsonl --verbose
  python3 parity_diff.py traces/x.jsonl --fields hp,energy,hand,monsters

退出码: 0=全通过, 1=有差异
"""
import argparse
import json
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.normpath(os.path.join(_here, "..", "..", "python")))

try:
    import neow_sim
except ImportError as e:
    print("neow_sim 未编译：", e)
    print("先跑 maturin develop 或把 .so 放到 neow-sim/python/")
    sys.exit(2)

# bridge 动作类型 → GymEnv action_type
BRIDGE_MAP = {
    "play": 0, "end_turn": 1, "potion": 2, "potion_discard": 3,
    "map": 4, "event": 5, "rest": 6, "reward": 8, "skip": 9,
    "proceed": 14, "cancel": 15, "card_pick": 10, "card_bowl": 11,
    "chest_open": 12, "hand_select": 16, "grid_select": 17,
    "scry": 18, "discovery": 19, "stance_choice": 20,
    "relic": 21, "boss": 22, "shop_enter": 13,
    "shop_buy_card": 7, "shop_buy_relic": 7, "shop_buy_potion": 7,
    "shop_card_removal": 7,
}

REST_OPTION = {"sleep": 0, "smith": 1, "toke": 2, "dig": 3, "lift": 4, "recall": 5}


def extract_entity(a):
    for key in ("card_index", "potion_index", "option_index", "relic_index", "index"):
        if key in a and a[key] is not None:
            return a[key]
    if "node_index" in a and a["node_index"] is not None:
        return a["node_index"]
    if "option" in a:
        return REST_OPTION.get(a["option"], 0)
    for key in ("indices", "take_indices"):
        if a.get(key):
            return a[key][0]
    return -1


def extract_target(a):
    t = a.get("target")
    return t if isinstance(t, int) else -1


def load(path):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


# ── 引擎状态导出 ────────────────────────────────────────────────
# GymEnv.info 目前是扁平 JSON 字符串；扩展自 pyo3 的 state_json() 后
# 这里直接读结构化状态。先做兼容降级。

def engine_snapshot(env):
    raw = env.info
    try:
        d = json.loads(raw)
    except Exception:
        d = {}
    out = {
        "hp": d.get("hp"),
        "max_hp": d.get("max_hp"),
        "gold": d.get("gold"),
        "act": d.get("act"),
        "floor": d.get("floor"),
        "state": d.get("state"),
    }
    sj = getattr(env, "state_json", None)
    if sj is not None:
        try:
            out.update(json.loads(sj()))
        except Exception:
            pass
    return out


# ── 比对 ────────────────────────────────────────────────────────

def norm_hand(hand):
    """bridge hand: [{id, upgraded, cost_for_turn}] ；引擎同形。"""
    out = []
    for c in hand or []:
        if isinstance(c, (list, tuple)):
            out.append((c[0], bool(c[1]) if len(c) > 1 else False,
                        c[2] if len(c) > 2 else None))
        else:
            out.append((c.get("id"), bool(c.get("upgraded")),
                        c.get("cost_for_turn", c.get("cost"))))
    return out


def norm_monsters(ms):
    out = []
    for m in ms or []:
        if m.get("is_gone"):
            continue
        pw = m.get("powers") or []
        if pw and isinstance(pw[0], dict):
            pw = sorted((p.get("id"), p.get("amount")) for p in pw)
        else:
            pw = sorted(tuple(p) for p in pw)
        it = m.get("intent") or {}
        out.append({
            "id": m.get("id"),
            "hp": m.get("hp"),
            "max_hp": m.get("max_hp"),
            "block": m.get("block", 0),
            "powers": pw,
            "intent_type": it.get("type"),
            "intent_dmg": it.get("damage"),
        })
    return out


def diff_state(want, got, fields, prefix=""):
    """返回差异列表 [(字段, 期望, 实际)]"""
    diffs = []

    def chk(name, w, g):
        if name not in fields:
            return
        if w != g:
            diffs.append((prefix + name, w, g))

    chk("hp", want.get("hp"), got.get("hp"))
    chk("max_hp", want.get("max_hp"), got.get("max_hp"))
    chk("gold", want.get("gold"), got.get("gold"))
    chk("floor", want.get("floor"), got.get("floor"))
    chk("act", want.get("act"), got.get("act"))

    if "player" in fields:
        wp, gp = want.get("player") or {}, got.get("player") or {}
        for k in ("hp", "max_hp", "block", "energy", "gold"):
            if wp.get(k) != gp.get(k):
                diffs.append((prefix + "player." + k, wp.get(k), gp.get(k)))

    if "hand" in fields:
        wh, gh = norm_hand(want.get("hand")), norm_hand(got.get("hand"))
        if wh != gh:
            diffs.append((prefix + "hand", wh, gh))

    if "monsters" in fields:
        wm, gm = norm_monsters(want.get("monsters")), norm_monsters(got.get("monsters"))
        if wm != gm:
            diffs.append((prefix + "monsters", wm, gm))

    if "piles" in fields:
        for k in ("draw_pile_n", "discard_pile_n", "exhaust_pile_n"):
            if want.get(k) != got.get(k):
                diffs.append((prefix + k, want.get(k), got.get(k)))

    if "relics" in fields:
        if want.get("relics") != got.get("relics"):
            diffs.append((prefix + "relics", want.get("relics"), got.get("relics")))

    if "rng" in fields:
        wr, gr = want.get("rng") or {}, got.get("rng") or {}
        for k in sorted(set(wr) | set(gr)):
            if wr.get(k) != gr.get(k):
                diffs.append((prefix + "rng." + k, wr.get(k), gr.get(k)))

    return diffs


def run_trace(path, fields, seed_override=None, verbose=False, max_report=40):
    rows = load(path)
    if not rows:
        return {"path": path, "error": "empty trace"}

    # 种子：trace 头部 meta 或文件名
    seed = seed_override
    asc = 0
    char = "IRONCLAD"
    for r in rows:
        if r.get("action") is None:
            st = r.get("state") or {}
            meta = st.get("meta") or {}
            seed = seed or meta.get("seed")
            asc = meta.get("ascension", asc)
    if seed is None:
        # 从文件名猜：traces/<seed>_<char>.jsonl
        base = os.path.basename(path).split("_")[0]
        seed = base
    asc = int(asc or 0)

    # 种子字符串 → u64
    if isinstance(seed, str) and not seed.isdigit():
        seed_u64 = neow_sim.seed_decode(seed) if hasattr(neow_sim, "seed_decode") else \
            int.from_bytes(seed.encode(), "little") & 0xFFFFFFFFFFFFFFFF
    else:
        seed_u64 = int(seed)

    env = neow_sim.GymEnv(seed=seed_u64, ascension=asc, character=char)

    diffs_all = []
    steps = 0
    skipped = 0
    for r in rows:
        a = r.get("action")
        if a is None:
            continue
        ty = BRIDGE_MAP.get(a.get("type"))
        if ty is None:
            skipped += 1
            continue
        ent = extract_entity(a)
        tgt = extract_target(a)
        try:
            obs, rew, done, info = env.step(ty, ent, tgt)
        except Exception as e:
            diffs_all.append(("<step %d %s>" % (r.get("i"), a.get("type")), "ok", str(e)))
            break
        steps += 1

        want = r.get("state")
        if not want:
            continue
        got = engine_snapshot(env)
        d = diff_state(want, got, fields, prefix="[%d]" % r.get("i", steps))
        if d:
            diffs_all.extend(d)
            if len(diffs_all) >= max_report:
                break
        if done:
            break

    return {
        "path": path,
        "steps": steps,
        "skipped": skipped,
        "diffs": diffs_all,
        "ok": not diffs_all,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("traces", nargs="+")
    ap.add_argument("--fields", default="hp,max_hp,gold,player,hand,monsters",
                    help="逗号分隔：hp,max_hp,gold,floor,act,player,hand,monsters,piles,relics,rng")
    ap.add_argument("--seed", default=None)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    fields = set(x.strip() for x in args.fields.split(",") if x.strip())
    total_ok = total_fail = 0
    for path in args.traces:
        r = run_trace(path, fields, args.seed, args.verbose)
        if r.get("error"):
            print("%-40s ERROR %s" % (os.path.basename(path), r["error"]))
            total_fail += 1
            continue
        if r["ok"]:
            print("%-40s PASS  %d steps" % (os.path.basename(path), r["steps"]))
            total_ok += 1
        else:
            print("%-40s FAIL  %d steps, %d diffs" %
                  (os.path.basename(path), r["steps"], len(r["diffs"])))
            for f, w, g in r["diffs"][:20]:
                print("    %-28s want=%r got=%r" % (f, w, g))
            total_fail += 1

    print("\n=== %d PASS / %d FAIL ===" % (total_ok, total_fail))
    return 0 if total_fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
