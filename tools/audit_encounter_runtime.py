#!/usr/bin/env python3
"""对拍：遭遇构造链的运行期 miscRng 消耗 + 怪物列表（真游戏模型 vs 引擎 enc_dump）。

真游戏模型直接照 MonsterHelper.java / 各怪物构造器写（独立于引擎实现），
用同一 XS128 实现跑 miscRng / monsterHpRng，比较：
  怪物种类序列、misc 消耗次数、monsterHp 消耗次数。

用法: python3 tools/audit_encounter_runtime.py
依赖: cargo（跑 crates/core/examples/enc_dump.rs）
"""
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
M = (1 << 64) - 1


def murmur(x):
    x &= M
    x ^= x >> 33
    x = (x * 0xFF51AFD7ED558CCD) & M
    x ^= x >> 33
    x = (x * 0xC4CEB9FE1A85EC53) & M
    x ^= x >> 33
    return x


class XS128:
    def __init__(self, seed):
        s0 = murmur(seed if seed else (1 << 63))
        self.s0, self.s1 = s0, murmur(s0)
        self.counter = 0

    def next_long(self):
        s0, s1 = self.s1, self.s0
        self.s0 = s0
        s1 ^= (s1 << 23) & M
        self.s1 = (s1 ^ s0 ^ (s1 >> 17) ^ (s0 >> 26)) & M
        return (self.s1 + s0) & M

    def next_int_n(self, n):
        while True:
            bits = self.next_long() >> 1
            v = bits % n
            sb = bits - (1 << 64) if bits >= (1 << 63) else bits
            if sb - v + (n - 1) >= 0:
                return v

    def rand(self, a, b):          # Random.random(int start,int end)
        self.counter += 1
        return a + self.next_int_n(b - a + 1)

    def rbool(self):               # Random.randomBoolean()
        self.counter += 1
        return (self.next_long() & 1) != 0


# ── 怪物构造器的 monsterHp 消耗次数（来自 audit_monster_rolls.py 的结论）──
NO_HP_ROLL = {"Transient", "Maw", "SphericGuardian"}     # super 常量
TWO_HP_ROLL = {"Taskmaster", "BronzeOrb", "OrbWalker", "TorchHead",       # super-random + setHp
               "LouseNormal", "LouseDefensive", "Darkling"}               # setHp + 额外掷
# 其余 = 1 次（setHp 或 super-random）


def hp_rolls(cls):
    """每只怪构造时的 monsterHpRng 消耗次数（同一时刻只跑一个进阶分支）。"""
    if cls in NO_HP_ROLL:
        return 0
    if cls in TWO_HP_ROLL:
        return 2      # super-random + setHp / setHp + bite / nip + setHp
    return 1


GREMLIN_POOL = ["GremlinWarrior", "GremlinWarrior", "GremlinThief", "GremlinThief",
                "GremlinFat", "GremlinFat", "GremlinTsundere", "GremlinWizard"]
SLIME_S_POOL = ["SpikeSlime_S", "SpikeSlime_S", "SpikeSlime_S", "AcidSlime_S", "AcidSlime_S"]
SHAPE_POOL = ["Repulsor", "Repulsor", "Exploder", "Exploder", "Spiker", "Spiker"]


class Model:
    """真游戏 MonsterHelper 构造链（独立实现）"""

    def __init__(self, seed=1585262650280):
        # 引擎里 resolve_encounter 用 run.rng 的当前状态；此处全新 run 即 counter=0
        self.misc = XS128(seed)
        self.hp = XS128(seed)

    # — 单只构造（只关心掷点与种类）—
    def spawn(self, cls, out, add=True):
        for _ in range(hp_rolls(cls)):
            self.hp.rand(0, 0)         # 仅推进计数（真界不同但消耗次数一致）
        if add:
            out.append(cls)

    def get_louse(self, out):
        if self.misc.rbool():
            self.spawn("LouseNormal", out)
        else:
            self.spawn("LouseDefensive", out)

    def bottom_get_weak_wildlife(self, out):
        b = self.misc.rbool()                       # getLouse 的种类掷
        louse = "LouseNormal" if b else "LouseDefensive"
        cands = [(louse, 2 if louse in ("LouseNormal", "LouseDefensive") else 1),
                 ("SpikeSlime_M", 1), ("AcidSlime_M", 1)]
        # 候选全构造（未选中者也掷 HP）
        for cls, n in cands:
            for _ in range(n):
                self.hp.rand(0, 0)
        pick = self.misc.rand(0, 2)
        out.append(cands[pick][0])

    def bottom_get_strong_wildlife(self, out):
        self.spawn("FungiBeast", [], add=False)   # 构造但不入 out
        self.spawn("JawWorm", [], add=False)
        pick = self.misc.rand(0, 1)
        out.append("FungiBeast" if pick == 0 else "JawWorm")

    def bottom_get_strong_humanoid(self, out):
        self.spawn("Cultist", [], add=False)
        b = self.misc.rbool()                  # getSlaver
        self.spawn("SlaverRed" if b else "SlaverBlue", [], add=False)
        self.spawn("Looter", [], add=False)
        pick = self.misc.rand(0, 2)
        out.append(["Cultist", "SlaverRed" if b else "SlaverBlue", "Looter"][pick])

    def spawn_gremlin(self, out):
        pick = self.misc.rand(0, 7)
        self.spawn(GREMLIN_POOL[pick], out)

    def run(self, name):
        out = []
        if name in ("2 Louse", "3 Louse"):
            for _ in range(2 if name == "2 Louse" else 3):
                self.get_louse(out)
        elif name == "Small Slimes":
            if self.misc.rbool():
                self.spawn("SpikeSlime_S", out)
                self.spawn("AcidSlime_M", out)
            else:
                self.spawn("AcidSlime_S", out)
                self.spawn("SpikeSlime_M", out)
        elif name == "Lots of Slimes":
            pool = list(SLIME_S_POOL)
            for _ in range(5):
                i = self.misc.rand(0, len(pool) - 1)
                cls = pool.pop(i)
                self.spawn(cls, out)
        elif name == "Large Slime":
            if self.misc.rbool():
                self.spawn("AcidSlime_L", out)
            else:
                self.spawn("SpikeSlime_L", out)
        elif name == "Gremlin Gang":
            pool = list(GREMLIN_POOL)
            for _ in range(4):
                i = self.misc.rand(0, len(pool) - 1)
                self.spawn(pool.pop(i), out)
        elif name == "Exordium Thugs":
            self.bottom_get_weak_wildlife(out)
            self.bottom_get_strong_humanoid(out)
        elif name == "Exordium Wildlife":
            self.bottom_get_strong_wildlife(out)
            self.bottom_get_weak_wildlife(out)
        elif name == "Gremlin Leader":
            self.spawn_gremlin(out)
            self.spawn_gremlin(out)
            self.spawn("GremlinLeader", out)
        elif name in ("3 Shapes", "4 Shapes"):
            n = 3 if name == "3 Shapes" else 4
            pool = list(SHAPE_POOL)
            for _ in range(n):
                i = self.misc.rand(0, len(pool) - 1)
                self.spawn(pool.pop(i), out)
        elif name == "Jaw Worm Horde":
            for _ in range(3):
                self.spawn("JawWorm", out)
        elif name == "2 Fungi Beasts":
            self.spawn("FungiBeast", out)
            self.spawn("FungiBeast", out)
        else:
            # 静态遭遇表：名字 → 怪物序列（与 MonsterHelper 一一对应）
            static = {
                "Blue Slaver": ["SlaverBlue"], "Cultist": ["Cultist"], "Jaw Worm": ["JawWorm"],
                "Looter": ["Looter"], "Red Slaver": ["SlaverRed"],
                "3 Byrds": ["Byrd"] * 3, "4 Byrds": ["Byrd"] * 4,
                "Chosen": ["Chosen"], "Shell Parasite": ["ShelledParasite"],
                "Spheric Guardian": ["SphericGuardian"],
                "Cultist and Chosen": ["Cultist", "Chosen"],
                "3 Cultists": ["Cultist"] * 3,
                "Chosen and Byrds": ["Byrd", "Chosen"],
                "Sentry and Sphere": ["Sentry", "SphericGuardian"],
                "Snake Plant": ["SnakePlant"], "Snecko": ["Snecko"],
                "Centurion and Healer": ["Centurion", "Healer"],
                "Shelled Parasite and Fungi": ["ShelledParasite", "FungiBeast"],
                "Book of Stabbing": ["BookOfStabbing"],
                "Slavers": ["SlaverBlue", "Taskmaster", "SlaverRed"],
                "Masked Bandits": ["BanditPointy", "BanditLeader", "BanditBear"],
                "Colosseum Nobs": ["Taskmaster", "GremlinNob"],
                "Colosseum Slavers": ["SlaverBlue", "SlaverRed"],
                "2 Thieves": ["Looter", "Mugger"],
                "3 Darklings": ["Darkling"] * 3,
                "Jaw Worm Horde": ["JawWorm"] * 3,
                "Snecko and Mystics": ["Healer", "Snecko", "Healer"],
                "Orb Walker": ["OrbWalker"], "Spire Growth": ["SpireGrowth"],
                "Maw": ["Maw"], "Transient": ["Transient"],
                "Flame Bruiser 1 Orb": ["Reptomancer", "SnakeDagger"],
                "Flame Bruiser 2 Orb": ["SnakeDagger", "Reptomancer", "SnakeDagger"],
                "Gremlin Nob": ["GremlinNob"], "Lagavulin": ["Lagavulin"],
                "3 Sentries": ["Sentry"] * 3,
                "2 Fungi Beasts": ["FungiBeast"] * 2,
            }
            if name not in static:
                return None
            for cls in static[name]:
                self.spawn(cls, out)
        return out


def engine_dump():
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + os.pathsep + env.get("PATH", "")
    p = subprocess.run(["cargo", "run", "-p", "neow-sim-core", "--quiet",
                        "--example", "enc_dump"], cwd=ROOT,
                       capture_output=True, text=True, env=env)
    if p.returncode != 0:
        sys.exit("enc_dump 失败:\n" + p.stderr[-2000:])
    out = {}
    for line in p.stdout.strip().splitlines():
        name, kinds, misc, hp = line.split("\t")
        kinds = re.findall(r"[A-Z]\w+", kinds)
        out[name] = (kinds, int(misc.split("=")[1]), int(hp.split("=")[1]))
    return out


def main():
    eng = engine_dump()
    bad = []
    for name, (ekinds, emisc, ehp) in eng.items():
        m = Model()
        rkinds = m.run(name)
        if rkinds is None:
            print(f"{name}: 模型未覆盖（跳过）")
            continue
        rmisc, rhp = m.misc.counter, m.hp.counter
        # 种类名对照（引擎 kind → 真类名）
        conv = {"LouseNormal": "LouseNormal", "LouseDefensive": "LouseDefensive"}
        norm = lambda xs: [x.replace("_", "") for x in xs]
        ok = (rmisc == emisc) and (rhp == ehp) and (norm(rkinds) == norm(ekinds))
        print(f"{name:28s} misc 真={rmisc:<2} 引擎={emisc:<2} hp 真={rhp:<2} 引擎={ehp:<2} "
              f"{'OK' if ok else '❌'}")
        if not ok:
            bad.append((name, rkinds, ekinds, rmisc, emisc, rhp, ehp))
    print(f"\n不一致 {len(bad)} 处")
    for b in bad:
        print(f"  {b[0]}: 怪物 真={b[1]} 引擎={b[2]} | misc {b[3]} vs {b[4]} | hp {b[5]} vs {b[6]}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
