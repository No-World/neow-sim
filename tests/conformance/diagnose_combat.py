# -*- coding: utf-8 -*-
"""诊断首个战斗分歧：逐战斗步打印 bridge 动作 / 双方手牌 / 双方怪物 HP。

用法: python3 tests/conformance/diagnose_combat.py traces/pmcwkzef_n1.jsonl [--max 15]
"""
import json
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _here)
sys.path.insert(0, os.path.normpath(os.path.join(_here, "..", "..", "python")))

import neow_sim                     # noqa: E402
from parity_diff import normalize, extract_entity, extract_target, BRIDGE_MAP  # noqa: E402

MAP_WIDTH = 7


def escreen(env):
    return json.loads(env.state_json()).get("state", "")


def brief(d):
    hand = [c.get("id") if isinstance(c, dict) else c[0] for c in (d.get("hand") or [])]
    mons = [(m.get("id"), m.get("hp")) for m in (d.get("monsters") or [])
            if (m.get("hp") or 0) > 0]
    p = d.get("player") or {}
    return "hp=%s/%s en=%s blk=%s hand=%s mons=%s" % (
        p.get("hp"), p.get("max_hp"), p.get("energy"), p.get("block"),
        [h[:7] for h in hand], mons)


def main():
    path = sys.argv[1]
    max_steps = int(sys.argv[3]) if len(sys.argv) > 3 else 15
    rows = [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]

    seed = None
    for r in rows:
        meta = (r.get("state") or {}).get("meta") or {}
        if meta.get("seed"):
            seed = meta["seed"]
            break
    env = neow_sim.GymEnv(seed=int(seed or 42), ascension=0, character="IRONCLAD")
    env.reset()

    prev = None
    shown = 0
    for r in rows:
        a = r.get("action")
        want = normalize(r.get("state")) or {}
        if a is None:
            prev = want.get("screen")
            continue
        t = a.get("type")

        is_combat_step = (prev == "combat"
                          and t in ("play", "end_turn", "potion", "potion_discard"))
        if is_combat_step and shown >= max_steps:
            break

        # 翻译推进
        s = escreen(env)
        try:
            if s == "NeowChoice":
                if t == "event" and prev == "event":
                    n = len(env.neow_options)
                    if n > 1:
                        env.step(5, extract_entity(a), -1)
            elif s == "MapNode":
                if t == "map":
                    col = int(extract_entity(a)) % MAP_WIDTH
                    for act in env.legal_actions.flat:
                        if act.action_type == 4 and (act.entity == col or act.target == col):
                            env.step(4, act.entity, -1)
                            break
                    else:
                        la = env.legal_actions.flat
                        if la:
                            env.step(4, la[0].entity, -1)
            elif s == "Combat":
                if t in BRIDGE_MAP and t in ("play", "end_turn", "potion", "potion_discard"):
                    env.step(BRIDGE_MAP[t], extract_entity(a), extract_target(a))
            else:
                if t in BRIDGE_MAP:
                    env.step(BRIDGE_MAP[t], extract_entity(a), extract_target(a))
        except Exception as e:
            print("   !! engine 推进失败: %s" % e)
            break

        if is_combat_step:
            shown += 1
            got = json.loads(env.state_json())
            b, g = brief(want), brief(got)
            mark = "  " if b == g else "≠ "
            print("%sstep %-3s %s" % (mark, r.get("i"), json.dumps(a, ensure_ascii=False)[:70]))
            if b != g:
                print("     bridge :", b)
                print("     engine :", g)
        prev = want.get("screen")
    return 0


if __name__ == "__main__":
    sys.exit(main())
