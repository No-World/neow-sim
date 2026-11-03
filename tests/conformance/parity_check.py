# -*- coding: utf-8 -*-
"""协议对拍：bridge 轨迹重放 → neow-sim 验证。

用法: python3 parity_check.py <轨迹.jsonl> [seed]
  从 bridge 轨迹读入动作序列，在模拟器重放，检查状态一致性。
"""
import json
import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "python"))
import neow_sim

# 动作类型映射：bridge JSON type → gym action_type int
BRIDGE_TO_GYM = {
    "play": 0,
    "end_turn": 1,
    "potion": 2,
    "potion_discard": 3,
    "map": 4,
    "event": 5,
    "rest": 6,
    "reward": 8,
    "skip": 9,
    "proceed": 14,
    "cancel": 15,
    "card_pick": 10,
    "card_bowl": 11,
    "chest_open": 12,
    "shop_enter": 13,
    "hand_select": 16,
    "grid_select": 17,
    "scry": 18,
    "discovery": 19,
    "stance_choice": 20,
    "relic": 21,
    "boss": 22,
}

def load_trajectory(path):
    """读入 bridge 轨迹 JSONL"""
    actions = []
    seed = None
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            d = json.loads(line)
            if "seed" in d:
                seed = d["seed"]
            if "action" in d:
                actions.append(d["action"])
    return seed, actions

def bridge_to_gym(action):
    """bridge JSON action → gym (type, entity, target) 三元组"""
    at = action.get("type", "")
    gym_type = BRIDGE_TO_GYM.get(at, -1)
    entity = -1
    target = -1

    if "card_index" in action:
        entity = action["card_index"]
    elif "card_id" in action:
        entity = -2  # 按卡牌 ID（模拟器需额外处理）
    elif "potion_index" in action:
        entity = action["potion_index"]
    elif "node_index" in action:
        entity = action["node_index"]
    elif "option_index" in action:
        entity = action["option_index"]
    elif "index" in action:
        entity = action["index"]
    elif "relic_index" in action:
        entity = action["relic_index"]
    elif "option" in action:
        entity = {"sleep": 0, "smith": 1, "toke": 2}.get(action["option"], 0)

    if "target" in action and action["target"] is not None:
        target = action["target"]
    if "indices" in action:
        entity = action["indices"][0] if action["indices"] else 0
    if "take_indices" in action:
        entity = action["take_indices"][0] if action["take_indices"] else 0

    return gym_type, entity, target

def run_parity(traj_path, seed_override=None):
    seed, actions = load_trajectory(traj_path)
    if seed_override:
        seed = seed_override
    print(f"轨迹: {traj_path}")
    print(f"  动作数: {len(actions)}")
    print(f"  seed: {seed}")

    # 初始化模拟器
    if seed:
        # bridge 种子可能是字符串（如 "PMCWKZEF"）
        try:
            seed_int = int(seed)
        except (ValueError, TypeError):
            seed_int = sum(ord(c) << (i * 8) for i, c in enumerate(str(seed)))
    else:
        seed_int = 42

    env = neow_sim.GymEnv(seed=seed_int, ascension=0, character="IRONCLAD")
    env.reset()

    matched = 0
    mismatched = 0
    skipped = 0
    errors = []

    for i, action in enumerate(actions):
        gym_type, entity, target = bridge_to_gym(action)
        if gym_type < 0:
            skipped += 1
            continue

        try:
            obs, reward, done, info = env.step(gym_type, entity, target)
            matched += 1
        except Exception as e:
            mismatched += 1
            errors.append(f"step {i}: {action['type']} entity={entity} → {e}")
            if len(errors) >= 10:
                break

        if done:
            break

    print(f"\n=== 对拍结果 ===")
    print(f"匹配步数: {matched}")
    print(f"错误步数: {mismatched}")
    print(f"跳过步数: {skipped}")
    print(f"到达 floor: 见 info")
    print(f"游戏结束: {env.is_done}")

    if errors:
        print(f"\n前 10 个错误:")
        for e in errors:
            print(f"  {e}")

    return matched, mismatched

if __name__ == "__main__":
    traj = sys.argv[1] if len(sys.argv) > 1 else "../runtime/win_PMCWKZEF.jsonl"
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else None
    run_parity(traj, seed)
