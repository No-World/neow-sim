# -*- coding: utf-8 -*-
"""协议对拍：bridge 轨迹重放 → neow-sim 逐步验证。

用法:
  python3 parity_check.py                              # 全部轨迹
  python3 parity_check.py win_PMCWKZEF.jsonl           # 单条轨迹
"""
import json
import os
import sys

# 添加 .so 路径
_here = os.path.dirname(os.path.abspath(__file__))
_so_dir = os.path.join(_here, "..", "..", "python")
if _so_dir not in sys.path:
    sys.path.insert(0, _so_dir)

import neow_sim

# bridge JSON type → gym action_type int
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

def extract_entity(a):
    """从 bridge JSON action 提取 entity 值"""
    for key in ["card_index", "potion_index", "option_index", "relic_index", "index"]:
        if key in a:
            return a[key]
    if "node_index" in a:
        return a["node_index"]
    if "option" in a:
        return {"sleep": 0, "smith": 1, "toke": 2, "dig": 3, "lift": 4}.get(a["option"], 0)
    if "indices" in a and a["indices"]:
        return a["indices"][0]
    if "take_indices" in a and a["take_indices"]:
        return a["take_indices"][0]
    return -1

def extract_target(a):
    t = a.get("target")
    if t is not None and isinstance(t, int):
        return t
    return -1

def load_traj(path):
    actions = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            d = json.loads(line.strip())
            if "action" in d:
                actions.append(d["action"])
    return actions

def seed_str_to_int(s):
    try:
        return int(s)
    except (ValueError, TypeError):
        return sum(ord(c) << ((i % 4) * 8) for i, c in enumerate(s))

def run_parity(traj_path, verbose=False):
    actions = load_traj(traj_path)
    seed_int = 42  # bridge 轨迹不含 seed——用固定值（模拟器本身确定性由 seed 控制）

    env = neow_sim.GymEnv(seed=seed_int, ascension=0, character="IRONCLAD")
    env.reset()

    matched = 0
    errors = 0
    skipped = 0
    error_msgs = []

    for i, a in enumerate(actions):
        at = a.get("type", "")
        gym_ty = BRIDGE_MAP.get(at, -1)
        if gym_ty < 0:
            skipped += 1
            continue

        entity = extract_entity(a)
        target = extract_target(a)

        try:
            obs, reward, done, info = env.step(gym_ty, entity, target)
            matched += 1
            if verbose:
                print(f"  [{i:3d}] {at:20s} e={entity:3d} t={target:3d} "
                      f"screen={obs.screen} seq={obs.seq_len} hp={env.info[:40]}")
        except Exception as e:
            errors += 1
            error_msgs.append(f"[{i:3d}] {at:20s} e={entity} t={target}: {e}")
            if errors >= 10:
                break

        if done:
            break

    return {
        "total": len(actions),
        "matched": matched,
        "errors": errors,
        "skipped": skipped,
        "error_msgs": error_msgs,
        "reached_step": matched + skipped + errors,
    }

if __name__ == "__main__":
    bridge_runtime = os.path.normpath(os.path.join(
        os.path.dirname(os.path.abspath(__file__)),
        "..", "..", "..", "neow-bridge", "runtime"
    ))
    bridge_runtime = os.path.normpath(bridge_runtime)

    if len(sys.argv) > 1 and sys.argv[1].endswith(".jsonl"):
        # 单条轨迹
        result = run_parity(sys.argv[1])
        print(f"\n  total={result['total']} matched={result['matched']} "
              f"errors={result['errors']} skipped={result['skipped']}")
        if result["error_msgs"]:
            print("Errors:")
            for m in result["error_msgs"]:
                print(f"  {m}")
    else:
        # 全部轨迹
        traj_dir = bridge_runtime
        total_pass = 0
        total_fail = 0
        for fname in sorted(os.listdir(traj_dir)):
            if not fname.startswith("win_") or not fname.endswith(".jsonl"):
                continue
            path = os.path.join(traj_dir, fname)
            r = run_parity(path)
            status = "PASS" if r["errors"] == 0 else "FAIL"
            print(f"{fname}: {r['matched']}/{r['total']} matched, "
                  f"{r['errors']} errors → {status}")
            if r["errors"] > 0:
                total_fail += 1
                for m in r["error_msgs"][:3]:
                    print(f"    {m}")
            else:
                total_pass += 1
        print(f"\n=== Summary: {total_pass} PASS / {total_fail} FAIL ===")
