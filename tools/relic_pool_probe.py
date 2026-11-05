#!/usr/bin/env python3
"""复刻真游戏遗物预洗池（Java HashMap 迭代序 + Collections.shuffle）。

用法: python3 tools/relic_pool_probe.py [--engine-order]
验证目标: floor7 elite 第 1 只 UNCOMMON = Mercury Hourglass，第 2 只 = Pear。
"""
import json
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
D = "/mnt/d/Users/NoWorld/Documents/source/Neow/.tools/decompiled-full"
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
        s0 = murmur(seed if seed != 0 else (1 << 63))
        s1 = murmur(s0)
        self.s0, self.s1 = s0, s1

    def next_long(self):
        s0 = self.s1
        s1 = self.s0
        self.s0 = s0
        s1 ^= (s1 << 23) & M
        self.s1 = (s1 ^ s0 ^ (s1 >> 17) ^ (s0 >> 26)) & M
        v = (self.s1 + s0) & M
        return v - (1 << 64) if v >= (1 << 63) else v

    def next_int_n(self, n):
        while True:
            bits = (self.next_long() & M) >> 1
            value = bits % n
            sb = bits - (1 << 63) if bits >= (1 << 63) else bits
            if sb - value + (n - 1) >= 0:
                return value


def jhash(s):
    h = 0
    for ch in s:
        h = (31 * h + ord(ch)) & 0xFFFFFFFF
    return h


class JHashMap:
    """Java 8 HashMap 迭代序模拟（String 键，无树化假设）"""

    def __init__(self):
        self.cap = 16
        self.thresh = 12
        self.table = {}
        self.size = 0

    def _bucket(self, key, cap):
        h = jhash(key)
        return (h ^ (h >> 16)) & (cap - 1)

    def put(self, key):
        if any(key in b for b in self.table.values()):
            return
        b = self._bucket(key, self.cap)
        self.table.setdefault(b, []).append(key)
        self.size += 1
        if self.size > self.thresh:
            self._resize()

    def _resize(self):
        old = [k for bidx in sorted(self.table) for k in self.table[bidx]]
        self.cap *= 2
        self.thresh = int(self.cap * 0.75)
        self.table = {}
        for k in old:
            b = self._bucket(k, self.cap)
            self.table.setdefault(b, []).append(k)

    def iteration_order(self):
        out = []
        for bidx in sorted(self.table):
            out.extend(self.table[bidx])
        return out


JM = (1 << 48) - 1


class JavaRandom:
    def __init__(self, seed):
        self.seed = (seed ^ 0x5DEECE66D) & JM

    def next(self, bits):
        self.seed = (self.seed * 0x5DEECE66D + 0xB) & JM
        return self.seed >> (48 - bits)

    def next_int_bound(self, bound):
        m = bound - 1
        r = self.next(31)
        if (bound & m) == 0:
            return (bound * r) >> 31
        u = r
        while True:
            r = u % bound
            if u - r + m >= 0:
                return r
            u = self.next(31)


def java_shuffle(lst, rnd):
    for i in range(len(lst) - 1, 0, -1):
        j = rnd.next_int_bound(i + 1)
        lst[i], lst[j] = lst[j], lst[i]


def load_insert_orders():
    """RelicLibrary 静态块 add/addRed 顺序 → 各类 → relicId"""
    rl = open(f"{D}/com/megacrit/cardcrawl/helpers/RelicLibrary.java").read()
    adds = re.findall(
        r"RelicLibrary\.(add|addRed|addGreen|addBlue|addPurple)\(new (\w+)\(\)\);", rl
    )
    shared, red = [], []
    for kind, cls in adds:
        p = f"{D}/com/megacrit/cardcrawl/relics/{cls}.java"
        src = open(p).read()
        m2 = re.search(r'String ID = "([^"]+)"', src)
        rid = m2.group(1) if m2 else None
        if rid is None:
            m3 = re.search(r'super\(\s*"([^"]+)"', src)
            rid = m3.group(1) if m3 else None
        if rid is None:
            continue
        if kind == "add":
            shared.append(rid)
        elif kind == "addRed":
            red.append(rid)
    return shared, red


def main():
    engine_tiers = {}
    src = open(f"{ROOT}/crates/core/src/relic_data.rs").read()
    for m in re.finditer(r'\("([^"]+)",\s*[\w:]+,\s*"(\w+)"\)', src):
        engine_tiers[m.group(1)] = m.group(2)

    shared, red = load_insert_orders()
    sh = JHashMap()
    for k in shared:
        sh.put(k)
    rd = JHashMap()
    for k in red:
        rd.put(k)
    sh_order = sh.iteration_order()
    rd_order = rd.iteration_order()

    seed = 1585262650280
    rng = XS128(seed)
    seeds = [rng.next_long() for _ in range(5)]
    roll6 = rng.next_int_n(100)
    print(f"#6 tier roll = {roll6} → {'COMMON' if roll6<50 else ('RARE' if roll6>82 else 'UNCOMMON')}")

    pools = {}
    for name, s in zip(("COMMON", "UNCOMMON", "RARE", "SHOP", "BOSS"), seeds):
        pool = [k for k in sh_order if engine_tiers.get(k) == name] + [
            k for k in rd_order if engine_tiers.get(k) == name
        ]
        java_shuffle(pool, JavaRandom(s & M))
        pools[name] = pool
    u = pools["UNCOMMON"]
    print(f"UNCOMMON 全序（{len(u)} 只）:")
    for i, k in enumerate(u):
        mark = ""
        if k == "Mercury Hourglass":
            mark = " ← 真游戏 elite#1"
        if k == "Pear":
            mark = " ← 真游戏 elite#2"
        print(f"  [{i:2d}] {k}{mark}")


if __name__ == "__main__":
    main()
