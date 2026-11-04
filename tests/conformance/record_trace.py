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

HOST, PORT = "127.0.0.1", int(os.environ.get("NEOW_BRIDGE_PORT", "37771"))
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

def select_indices(obs):
    """从 screen payload 读 num_to_select / can_pick_zero，给出要选的下标。

    hand_select / grid_select / scry 的 payload 形如：
      {"type":"hand_select","num_to_select":1,"max_to_select":1,"can_pick_zero":false}
    """
    scr = obs.get("screen") or {}
    n = scr.get("num_to_select")
    if n is None:
        n = scr.get("num", 0)
    n = int(n or 0)
    if n <= 0:
        return []
    # -1 表示「任意数量」；至少选 1 张
    if n < 0:
        n = 1
    return list(range(n))


# 非战斗屏的动作优先级：优先「推进流程」的动作，最后才 cancel
PREF = ("reward", "card_pick", "chest_open", "boss", "card_bowl",
        "grid_select", "hand_select", "scry", "discovery", "stance_choice",
        "event", "rest", "map", "shop_buy_card", "shop_buy_relic",
        "shop_buy_potion", "shop_card_removal", "proceed", "skip")


def pick_action(obs, info, legal=None):
    """返回 bridge action dict，或 None 表示不会再推进。

    ⚠️ 合法动作在 bridge 响应的 `result.legal_actions`，不在 observation 里。
    非战斗屏直接按 PREF 优先序从合法动作里挑，不要自己猜 node_index/index。
    """
    screen = info.get("screen", "")
    if legal is None:
        legal = obs.get("legal", []) or []
    if not legal:
        return None

    if screen == "combat":
        monsters = [m for m in (obs.get("monsters") or []) if not m.get("is_gone")]
        plays = [a for a in legal if a.get("type") == "play"]

        def score(a):
            cid = a.get("card_id", "")
            if "Strike" in cid or "Bash" in cid:
                return 0
            if "Defend" in cid:
                return 1
            return 2

        # 优先打攻击牌；没有可打的攻击再考虑其它牌；最后结束回合
        plays.sort(key=score)
        alive = [m for m in monsters if m.get("hp", 1) > 0]
        for a in plays:
            act = dict(a)
            if act.get("target") is None and alive:
                # 必须用怪物自带的 index（Obs 里 monsters[i].index），
                # 不能用过滤后列表的下标——死掉的怪仍占数组位置。
                tgt = min(alive, key=lambda m: m["hp"])
                act["target"] = tgt.get("index", 0)
            return act
        for a in legal:
            if a.get("type") == "end_turn":
                return dict(a)
        return dict(legal[0])

    # 非战斗：按优先序
    for want in PREF:
        for a in legal:
            if a.get("type") == want:
                act = dict(a)
                # 选择类动作需要 indices；没给就补 [0]（有可选牌时）
                if want in ("grid_select", "hand_select", "scry") and "indices" not in act:
                    act["indices"] = select_indices(obs)
                return act
    # 兜底：第一个不是 cancel 的
    for a in legal:
        if a.get("type") != "cancel":
            return dict(a)
    return dict(legal[0])


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
        "deck": [(c.get("id"), bool(c.get("upgraded")))
                 for c in ((obs.get("master_deck") or {}).get("cards") or [])],
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
