# -*- coding: utf-8 -*-
"""录制干净的真游戏轨迹：必须先 quit_run → 清存档 → new_run（单次，不重试）。

旧流程的问题：drive_win.py 会「同种子多次 new_run」，退出不彻底的局会把
master_deck 残留带到下一局（实测 PMCWKZEF 的 deck 里出现了 Power Through，
而按 Neow transform 的池/下标模型它不可能被抽到）——对拍会拿到假差异。

用法:
  python3 tests/conformance/record_clean.py --seed PMCWKZEF --asc 0 --char IRONCLAD \
          --out traces/pmcwkzef.jsonl --max-steps 300
"""
import argparse
import json
import os
import shutil
import subprocess
import sys

_here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _here)
sys.path.insert(0, os.path.normpath(os.path.join(_here, "..", "..")))

import record_trace as rt  # noqa: E402

BRIDGE = os.path.normpath(os.path.join(_here, "..", "..", "..", "neow-bridge"))
SAVES = os.path.join(BRIDGE, "runtime", "saves")


def clean_saves():
    """把 autosave 挪走（不删，便于查证），保证 new_run 是全新局。"""
    if not os.path.isdir(SAVES):
        return 0
    bak = os.path.join("/tmp", "neow_saves_bak")
    os.makedirs(bak, exist_ok=True)
    n = 0
    for f in os.listdir(SAVES):
        if "autosave" in f:
            src = os.path.join(SAVES, f)
            dst = os.path.join(bak, f)
            try:
                if os.path.exists(dst):
                    os.remove(dst)
                shutil.move(src, dst)
                n += 1
            except OSError as e:
                print("  [warn] 无法移动 %s: %s" % (f, e))
    return n


def _n_event_opts(obs):
    scr = obs.get("screen") or {}
    for key in ("options", "choices"):
        v = scr.get(key)
        if isinstance(v, list):
            return len(v)
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", required=True)
    ap.add_argument("--asc", type=int, default=0)
    ap.add_argument("--char", default="IRONCLAD")
    ap.add_argument("--out", default=None)
    ap.add_argument("--max-steps", type=int, default=300)
    ap.add_argument("--neow-idx", type=int, default=1,
                    help="Neow 选第几个祝福（0..3）。默认 1=Max HP+10%%："
                         "不动牌组，避免 ShowCardAndObtainEffect 在无动画帧下不结算"
                         "（transform 会让 deck 少一张）")
    args = ap.parse_args()

    rpc = rt.Rpc()
    rt_neow_done = [False]

    # 1) 若已在局内 → quit_run
    r = rpc.call("ping")
    if r.get("ok") and r.get("result", {}).get("in_dungeon"):
        print("已在局内 → quit_run", flush=True)
        print("  ", rpc.call("quit_run"), flush=True)

    # 2) 清存档
    print("清理 autosave: %d 个" % clean_saves(), flush=True)

    # 3) 单次 new_run（不重试）
    r = rpc.call("new_run", character=args.char, ascension=args.asc, seed=args.seed)
    if not r.get("ok"):
        sys.exit("new_run failed: %s" % r)
    print("new_run ok: seed=%s asc=%d char=%s" % (args.seed, args.asc, args.char), flush=True)

    # 4) 校验 Neow 起手（floor 0）
    st = rpc.call("get_state")["result"]
    info = st.get("info", {})
    obs = st["observation"]
    print("起始: screen=%s floor=%s deck=%d" % (
        info.get("screen"), (obs.get("meta") or {}).get("floor"),
        (obs.get("master_deck") or {}).get("count", -1)), flush=True)
    if info.get("screen") != "event":
        print("  [warn] 起手不是 event 屏，可能存档未清干净", flush=True)

    out = args.out or os.path.join("traces", "%s.jsonl" % args.seed.lower())
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)

    # 5) 录制（与 record_trace 同一逻辑，但复用已开好的局）
    n = 0
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"i": 0, "action": None, "state": rt.snapshot(st)},
                            ensure_ascii=False) + "\n")
        fh.flush()
        while n < args.max_steps:
            r = rpc.call("get_state")
            if not r.get("ok"):
                print("get_state err:", r.get("error"), flush=True)
                break
            st = r["result"]
            obs, info = st["observation"], st.get("info", {})
            if info.get("screen") == "game_over":
                print("game over", flush=True)
                break

            action = rt.pick_action(obs, info, st.get("legal_actions") or [])
            # Neow 祝福选择屏（4 个选项）强制用 --neow-idx
            if (info.get("screen") == "event"
                    and not rt_neow_done[0]
                    and _n_event_opts(obs) >= 4):
                action = {"type": "event", "option_index": args.neow_idx}
                rt_neow_done[0] = True
            if action is None:
                print("no action on screen=%s; stop" % info.get("screen"), flush=True)
                # 把当前屏记下来便于补分支
                fh.write(json.dumps({"i": n + 1, "action": None,
                                     "state": rt.snapshot(r), "stuck": True},
                                    ensure_ascii=False) + "\n")
                break

            r2 = rpc.call("step", action=action)
            n += 1
            rec = {"i": n, "action": action, "state": rt.snapshot(r2)}
            if not r2.get("ok"):
                rec["error"] = r2.get("error")
                fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
                fh.flush()
                print("step %d FAILED: %s" % (n, r2.get("error")), flush=True)
                break
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
            fh.flush()
            if n % 25 == 0:
                s = rec["state"] or {}
                print("  %d: %s floor=%s hp=%s" % (
                    n, s.get("screen"), s.get("floor"),
                    (s.get("player") or {}).get("hp")), flush=True)
            if info.get("screen") == "game_over":
                break

    rpc.close()
    print("wrote %d steps -> %s" % (n, out), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
