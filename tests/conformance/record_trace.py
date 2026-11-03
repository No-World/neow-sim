# -*- coding: utf-8 -*-
"""录制带状态的真游戏轨迹（golden trace）。

每步：执行动作 → get_state 快照 → 落盘。产物可直接喂给 parity_check.py 做逐字段 diff。

用法:
  python3 record_trace.py --seed PMCWKZEF --asc 0 --char IRONCLAD --out traces/pmcwkzef.jsonl --max-steps 300
  python3 record_trace.py --auto            # 用启发式策略自动打（无需人操作）

设计要点：
  - state 快照是「动作执行后」的状态，parity 侧按同一顺序重放即可逐步对齐
  - rng 计数器全部记录：一旦某流分叉，能立刻定位是哪一步哪个流
"""
import argparse
import json
import os
import socket
import sys
import time

HOST, PORT = "127.0.0.1", 37771
_id = [0]


class Rpc:
    def __init__(self, host=HOST, port=PORT):
        self.host, self.port = host, port
        self.sock = None
        self.f = None

    def connect(self):
        self.sock = socket.create_connection((self.host, self.port), timeout=200)
        self.f = self.sock.makefile("rwb")

    def call(self, cmd, **kw):
        for attempt in range(6):
            try:
                if self.f is None:
                    self.connect()
                _id[0] += 1
                req = {"id": _id[0], "cmd": cmd}
                req.update(kw)
                self.f.write((json.dumps(req) + "\n").encode())
                self.f.flush()
                line = self.f.readline()
                if not line:
                    raise ConnectionError("closed")
                return json.loads(line)
            except (ConnectionError, OSError, json.JSONDecodeError) as e:
                print("  [rpc] %s 重连(%d): %s" % (cmd, attempt + 1, e), flush=True)
                time.sleep(3)
                try:
                    self.close()
                    self.connect()
                except OSError:
                    time.sleep(5)
        raise RuntimeError("rpc failed: %s" % cmd)

    def close(self):
        try:
            if self.f:
                self.f.close()
            if self.sock:
                self.sock.close()
        except Exception:
            pass
        self.f = self.sock = None


# ── 动作选取（启发式，够用即可；目标是产出轨迹不是赢） ────────────

def pick_action(obs, info):
    """返回 bridge action dict，或 None 表示不会再推进。"""
    screen = info.get("screen", "")
    legal = obs.get("legal", []) or []

    if screen == "combat":
        # 优先打攻击牌打最低血怪；否则防御；否则结束回合
        monsters = [m for m in (obs.get("monsters") or []) if not m.get("is_gone")]
        plays = [a for a in legal if a.get("type") == "play"]
        if plays:
            def score(a):
                cid = a.get("card_id", "")
                if "Strike" in cid or "Bash" in cid:
                    return 0
                if "Defend" in cid:
                    return 1
                return 2
            plays.sort(key=score)
            a = dict(plays[0])
            if a.get("target") is None and monsters:
                a["target"] = min(range(len(monsters)), key=lambda i: monsters[i]["hp"])
            return a
        return {"type": "end_turn"}

    if screen == "map":
        nodes = obs.get("next_nodes") or obs.get("nodes") or []
        for i, n in enumerate(nodes):
            if n.get("available", True):
                return {"type": "map", "node_index": n.get("index", i)}
        return {"type": "map", "node_index": 0}

    if screen == "event":
        choices = obs.get("choices") or []
        for c in choices:
            if not c.get("disabled"):
                return {"type": "event", "option_index": c.get("index", 0)}
        return {"type": "event", "option_index": 0}

    if screen in ("reward", "card_reward"):
        if screen == "card_reward" or obs.get("cards"):
            return {"type": "card_pick", "index": 0}
        for r in legal:
            if r.get("type") in ("reward", "skip"):
                return dict(r)
        return {"type": "proceed"}

    if screen == "rest":
        return {"type": "rest", "option": "sleep"}

    if screen == "shop":
        return {"type": "proceed"}

    if screen == "boss_relic":
        return {"type": "boss", "index": 0}

    if screen == "treasure":
        return {"type": "chest_open"}

    for a in legal:
        return dict(a)
    return None


def snapshot(resp):
    """提取用于 diff 的规范状态。"""
    if not resp.get("ok"):
        return None
    res = resp.get("result", {})
    obs = res.get("observation")
    if obs is None:
        return None
    info = res.get("info", {})
    p = obs.get("player", {})
    return {
        "screen": info.get("screen"),
        "act": info.get("act"),
        "floor": info.get("floor"),
        "turn": obs.get("meta", {}).get("turn"),
        "meta": {
            "seed": obs.get("meta", {}).get("seed"),
            "seed_code": obs.get("meta", {}).get("seed_code"),
            "ascension": obs.get("meta", {}).get("ascension"),
            "character": obs.get("meta", {}).get("character"),
        },
        "player": {
            "hp": p.get("hp"),
            "max_hp": p.get("max_hp"),
            "block": p.get("block"),
            "energy": p.get("energy"),
            "gold": p.get("gold"),
            "powers": p.get("powers"),
        },
        "hand": [(c.get("id"), c.get("upgraded"), c.get("cost_for_turn"))
                 for c in (obs.get("hand") or [])],
        "monsters": [
            {
                "id": m.get("id"),
                "hp": m.get("hp"),
                "max_hp": m.get("max_hp"),
                "block": m.get("block"),
                "powers": m.get("powers"),
                "intent": m.get("intent"),
            }
            for m in (obs.get("monsters") or [])
        ],
        "draw_pile_n": (obs.get("draw_pile") or {}).get("count"),
        "discard_pile_n": (obs.get("discard_pile") or {}).get("count"),
        "exhaust_pile_n": (obs.get("exhaust_pile") or {}).get("count"),
        "relics": [(r.get("id"), r.get("counter")) for r in (obs.get("relics") or [])],
        "rng": {k: v.get("counter") if isinstance(v, dict) else v
                for k, v in (obs.get("rng") or {}).items()},
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", default=None, help="种子字符串（不给则沿用当前局）")
    ap.add_argument("--asc", type=int, default=0)
    ap.add_argument("--char", default="IRONCLAD")
    ap.add_argument("--out", default=None)
    ap.add_argument("--max-steps", type=int, default=400)
    ap.add_argument("--auto", action="store_true", help="自动打（启发式）")
    ap.add_argument("--no-record-state", action="store_true")
    args = ap.parse_args()

    rpc = Rpc()
    if args.seed:
        r = rpc.call("new_run", character=args.char, ascension=args.asc, seed=args.seed)
        if not r.get("ok"):
            print("new_run failed:", r)
            return 1
        print("new_run ok:", args.seed, flush=True)

    out = args.out or ("traces/%s_%s.jsonl" % (args.seed or "current", args.char.lower()))
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)

    n = 0
    with open(out, "w", encoding="utf-8") as fh:
        # 起始状态
        r = rpc.call("get_state")
        if r.get("ok"):
            fh.write(json.dumps({"i": 0, "action": None,
                                 "state": None if args.no_record_state else snapshot(r)},
                                ensure_ascii=False) + "\n")
            fh.flush()

        while n < args.max_steps:
            r = rpc.call("get_state")
            if not r.get("ok"):
                print("get_state err:", r.get("error"), flush=True)
                break
            obs = r["result"]["observation"]
            info = r["result"].get("info", {})
            if info.get("screen") == "game_over":
                print("game over", flush=True)
                break

            action = pick_action(obs, info)
            if action is None:
                print("no action; stopping", flush=True)
                break

            r2 = rpc.call("step", action=action)
            n += 1
            rec = {"i": n, "action": action,
                   "state": None if args.no_record_state else snapshot(r2)}
            if not r2.get("ok"):
                rec["error"] = r2.get("error")
                fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
                fh.flush()
                print("step %d FAILED: %s" % (n, r2.get("error")), flush=True)
                break
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
            fh.flush()
            if n % 20 == 0:
                p = rec["state"]["player"] if rec["state"] else {}
                print("  %d: %s floor=%s hp=%s" % (n, info.get("screen"), info.get("floor"), p.get("hp")),
                      flush=True)
            if r2.get("result", {}).get("info", {}).get("screen") == "game_over":
                break

    rpc.close()
    print("wrote %d steps -> %s" % (n, out), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
