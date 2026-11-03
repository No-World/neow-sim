# -*- coding: utf-8 -*-
"""生成链对拍：真游戏 jar（GenDump.java） ←→ neow-sim（cargo example gen_dump）。

对比项：地图边结构 / 房间类型 / 遭遇序列 / 精英 / Boss / Neow 4 选项 / RNG 计数器。
无需真游戏窗口——Java 侧直接调 jar 的 MapGenerator + MonsterInfo + Random。

用法:
  python3 tools/parity_gen.py                 # 默认 40 个种子
  python3 tools/parity_gen.py --seeds 1,42    # 指定种子
  python3 tools/parity_gen.py --count 200     # 随机 200 个种子
  python3 tools/parity_gen.py -v              # 打印每条差异
"""
import argparse
import os
import random
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, ".."))
JAVAGEN = os.path.normpath(os.path.join(ROOT, "..", ".tools", "javagen"))
JAR = os.path.normpath(os.path.join(ROOT, "..", "neow-bridge", "runtime", "desktop-1.0.jar"))

# 差异分类：先比「结构性」的，再比 RNG（RNG 往往是结构差异的结果）
CATEGORY_ORDER = ["map", "room", "enc monster", "enc elite", "boss", "neow", "rng"]


def run_java(seeds):
    cp = JAVAGEN + os.pathsep + JAR
    p = subprocess.run(
        ["java", "-cp", cp, "GenDump"] + [str(s) for s in seeds],
        capture_output=True, text=True,
    )
    if p.returncode != 0:
        sys.exit("GenDump failed:\n" + p.stderr[-3000:])
    return p.stdout


def run_rust(seeds):
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + os.pathsep + env.get("PATH", "")
    p = subprocess.run(
        ["cargo", "run", "--quiet", "--example", "gen_dump", "--"] + [str(s) for s in seeds],
        cwd=ROOT, capture_output=True, text=True, env=env,
    )
    if p.returncode != 0:
        sys.exit("gen_dump failed:\n" + p.stderr[-3000:])
    return p.stdout


KNOWN_PREFIX = ("seed ", "map ", "room ", "enc ", "boss ", "neow ", "rng ")


def parse(text):
    """→ {seed: [(kind, payload_str)]}，保持行序。

    真游戏侧 log4j 会把日志打到 stdout，这里只保留已知格式行。
    """
    out, cur = {}, None
    for line in text.splitlines():
        line = line.rstrip()
        if not line:
            continue
        if not (line == "end" or line.startswith(KNOWN_PREFIX)):
            continue
        if line.startswith("seed "):
            cur = int(line.split()[1])
            out[cur] = []
        elif line == "end":
            cur = None
        elif cur is not None:
            out[cur].append(line)
    return out


def group_by_kind(lines):
    """按 kind（首段/前两段）分组，返回 {kind: [rest_of_line]}"""
    g = {}
    for line in lines:
        parts = line.split(" ", 2)
        if parts[0] == "enc":
            key = "enc " + parts[1]
            rest = parts[2] if len(parts) > 2 else ""
        else:
            key = parts[0]
            rest = line[len(key) + 1:]
        g.setdefault(key, []).append(rest)
    return g


def diff_seed(seed, java_lines, rust_lines, verbose):
    gj, gr = group_by_kind(java_lines), group_by_kind(rust_lines)
    kinds = CATEGORY_ORDER + sorted((set(gj) | set(gr)) - set(CATEGORY_ORDER))
    diffs = []
    for k in kinds:
        a, b = gj.get(k, []), gr.get(k, [])
        if a == b:
            continue
        if k == "rng":
            diffs.append((k, "; ".join(a), "; ".join(b)))
            continue
        # 找第一处具体差异，便于定位
        sa, sb = set(a), set(b)
        only_j = sorted(sa - sb)[:3]
        only_r = sorted(sb - sa)[:3]
        msg = "java_only=%r rust_only=%r (n_java=%d n_rust=%d)" % (
            only_j, only_r, len(a), len(b))
        diffs.append((k, msg, ""))
    return diffs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", default=None, help="逗号分隔的种子列表")
    ap.add_argument("--count", type=int, default=40)
    ap.add_argument("--base", type=int, default=1, help="随机种子的下界")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--max-report", type=int, default=6, help="每个种子最多报几条差异")
    args = ap.parse_args()

    if args.seeds:
        seeds = [int(s) for s in args.seeds.split(",")]
    else:
        rnd = random.Random(20260420)
        seeds = [rnd.randint(args.base, 10**12) for _ in range(args.count)]

    java = parse(run_java(seeds))
    rust = parse(run_rust(seeds))

    total_ok = 0
    fail_by_kind = {}
    failed_seeds = []
    for s in seeds:
        d = diff_seed(s, java.get(s, []), rust.get(s, []), args.verbose)
        if not d:
            total_ok += 1
            continue
        failed_seeds.append(s)
        for k, _, _ in d:
            fail_by_kind[k] = fail_by_kind.get(k, 0) + 1
        print("FAIL seed=%d  %s" % (s, ", ".join(k for k, _, _ in d)))
        if args.verbose:
            for k, a, b in d[: args.max_report]:
                print("    [%s] %s" % (k, a))

    print("\n=== %d/%d seeds PASS ===" % (total_ok, len(seeds)))
    if fail_by_kind:
        print("按类别统计（有差异的种子数）：")
        for k, n in sorted(fail_by_kind.items(), key=lambda kv: -kv[1]):
            print("  %-14s %d" % (k, n))
    if failed_seeds and not args.verbose:
        print("\n加 -v 看具体差异；复现单个种子： --seeds %d -v" % failed_seeds[0])
    return 0 if total_ok == len(seeds) else 1


if __name__ == "__main__":
    sys.exit(main())
