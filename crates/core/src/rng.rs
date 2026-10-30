//! 确定性 RNG 层 —— 逐行复刻游戏本体实现，是 M4 对拍的地基。
//!
//! 来源（反编译，见 docs/engine-semantics.md §5 与 .tools/decompiled/）：
//! - `Xs128`   ← libgdx `com.badlogic.gdx.math.RandomXS128`（xorshift128+）
//! - `StsRandom` ← `com.megacrit.cardcrawl.random.Random`（带 counter 的包装层）
//! - `JavaRandom`/`java_shuffle` ← `java.util.Random` + `Collections.shuffle`
//!   （`CardGroup.shuffle()` = 从 STS 流取 1 个 long 种子化 java.util.Random 再洗牌）
//!
//! 黄金向量由 `.tools/javagen/GoldenGen.java` 直接调用游戏 jar 内的真实现生成，
//! 存于 `tests/golden_rng.txt`，测试逐值对拍。

/// murmurHash3 的 64 位 finalizer（RandomXS128.setSeed 用）
fn murmur_hash3(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    x
}

/// libgdx `RandomXS128` 的字节级复刻（xorshift128+，双 64 位状态）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Xs128 {
    seed0: u64,
    seed1: u64,
}

impl Xs128 {
    pub fn new(seed: u64) -> Self {
        // Java: seed == 0 时用 Long.MIN_VALUE
        let s0 = murmur_hash3(if seed == 0 { i64::MIN as u64 } else { seed });
        let s1 = murmur_hash3(s0);
        Self {
            seed0: s0,
            seed1: s1,
        }
    }

    pub fn from_state(seed0: u64, seed1: u64) -> Self {
        Self { seed0, seed1 }
    }

    /// 与 `getState(int)` 对齐：0 → seed0，1 → seed1
    pub fn get_state(&self, i: usize) -> u64 {
        if i == 0 {
            self.seed0
        } else {
            self.seed1
        }
    }

    /// xorshift128+ 主推进。Java 返回有符号 long，这里用 u64 承载同样的位型。
    pub fn next_long(&mut self) -> u64 {
        let mut s1 = self.seed0;
        let s0 = self.seed1;
        self.seed0 = s0;
        s1 ^= s1 << 23;
        self.seed1 = s1 ^ s0 ^ (s1 >> 17) ^ (s0 >> 26);
        self.seed1.wrapping_add(s0)
    }

    /// `nextLong(long n)`：62 位拒绝采样（`nextInt(int n)` 即 `(int)nextLong(n)`）
    pub fn next_long_n(&mut self, n: u64) -> u64 {
        assert!(n > 0, "n must be positive");
        loop {
            let bits = self.next_long() >> 1;
            let value = bits % n;
            // Java 用有符号溢出判断 (bits - value + n-1) < 0
            if (bits.wrapping_sub(value).wrapping_add(n - 1) as i64) >= 0 {
                return value;
            }
        }
    }

    pub fn next_int_n(&mut self, n: u64) -> u64 {
        self.next_long_n(n)
    }

    /// 注意：Java 是 `(double)(>>>11) * (double)1.110223E-16f` —— 常量先取 float 再展宽
    pub fn next_double(&mut self) -> f64 {
        ((self.next_long() >> 11) as f64) * (1.110_223e-16_f32 as f64)
    }

    #[allow(clippy::excessive_precision)] // 5.9604644775390625e-8 = 2^-24，保留与 Java 相同的字面量
    pub fn next_float(&mut self) -> f32 {
        ((self.next_long() >> 40) as f64 * 5.960_464_477_539_062_5e-8) as f32
    }

    pub fn next_boolean(&mut self) -> bool {
        (self.next_long() & 1) != 0
    }
}

/// STS 的 `com.megacrit.cardcrawl.random.Random`：包装 Xs128 + 调用计数器
/// （游戏存档会保存 counter，模拟器照抄以支持快照/回放）
#[derive(Clone, Debug)]
pub struct StsRandom {
    pub rng: Xs128,
    pub counter: u32,
}

impl StsRandom {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Xs128::new(seed),
            counter: 0,
        }
    }

    /// 快照导出（对应 getState(0)/getState(1) + counter）
    pub fn snapshot(&self) -> (u64, u64, u32) {
        (self.rng.get_state(0), self.rng.get_state(1), self.counter)
    }

    pub fn restore(seed0: u64, seed1: u64, counter: u32) -> Self {
        Self {
            rng: Xs128::from_state(seed0, seed1),
            counter,
        }
    }

    /// `random(int range)` = nextInt(range + 1)
    pub fn random_range(&mut self, range: i32) -> i32 {
        self.counter += 1;
        self.rng.next_long_n(range as u64 + 1) as i32
    }

    /// `random(int start, int end)` = start + nextInt(end - start + 1)
    pub fn random_start_end(&mut self, start: i32, end: i32) -> i32 {
        self.counter += 1;
        start + self.rng.next_long_n((end - start + 1) as u64) as i32
    }

    /// `random(long range)` = (long)(nextDouble * range)
    pub fn random_long_range(&mut self, range: i64) -> i64 {
        self.counter += 1;
        (self.rng.next_double() * range as f64) as i64
    }

    /// `random(long start, long end)` = start + (long)(nextDouble * (end - start))，注意无 +1
    pub fn random_long_start_end(&mut self, start: i64, end: i64) -> i64 {
        self.counter += 1;
        start + (self.rng.next_double() * (end - start) as f64) as i64
    }

    pub fn random_long(&mut self) -> i64 {
        self.counter += 1;
        self.rng.next_long() as i64
    }

    pub fn random_boolean(&mut self) -> bool {
        self.counter += 1;
        self.rng.next_boolean()
    }

    /// `randomBoolean(float chance)` = nextFloat() < chance
    pub fn random_boolean_chance(&mut self, chance: f32) -> bool {
        self.counter += 1;
        self.rng.next_float() < chance
    }

    pub fn random_float(&mut self) -> f32 {
        self.counter += 1;
        self.rng.next_float()
    }

    /// `setCounter`：只能向前快进（用 randomBoolean 推进，与游戏一致）
    pub fn set_counter(&mut self, target: u32) {
        while self.counter < target {
            self.random_boolean();
        }
    }
}

/// `java.util.Random` 的 48 位 LCG 复刻（洗牌种子化用）
#[derive(Clone, Debug)]
pub struct JavaRandom {
    seed: u64,
}

const JAVAR_MULT: u64 = 0x5DEECE66D;
const JAVAR_ADD: u64 = 0xB;
const JAVAR_MASK: u64 = (1u64 << 48) - 1;

impl JavaRandom {
    pub fn new(seed: i64) -> Self {
        Self {
            seed: ((seed as u64) ^ JAVAR_MULT) & JAVAR_MASK,
        }
    }

    fn next(&mut self, bits: u32) -> u32 {
        self.seed = (self.seed.wrapping_mul(JAVAR_MULT).wrapping_add(JAVAR_ADD)) & JAVAR_MASK;
        (self.seed >> (48 - bits)) as u32
    }

    /// `nextInt(bound)`：2 的幂走移位捷径，否则 31 位拒绝采样
    pub fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        let m = bound - 1;
        let mut r = self.next(31) as i32;
        if (bound & m) == 0 {
            r = ((bound as i64 * r as i64) >> 31) as i32;
        } else {
            let mut u = r;
            loop {
                r = u % bound;
                if u.wrapping_sub(r).wrapping_add(m) >= 0 {
                    break;
                }
                u = self.next(31) as i32;
            }
        }
        r
    }

    /// Java 的符号扩展语义：两个 next(32) 都按有符号 int 提升
    pub fn next_long(&mut self) -> i64 {
        let hi = self.next(32) as i32 as i64;
        let lo = self.next(32) as i32 as i64;
        hi.wrapping_shl(32).wrapping_add(lo)
    }

    pub fn next_boolean(&mut self) -> bool {
        self.next(1) != 0
    }

    pub fn next_float(&mut self) -> f32 {
        self.next(24) as f32 / (1u32 << 24) as f32
    }

    pub fn next_double(&mut self) -> f64 {
        let hi = self.next(26) as i64;
        let lo = self.next(27) as i64;
        ((hi << 27) + lo) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// `Collections.shuffle` 语义：i 从尾向前，j = nextInt(i+1)，swap
pub fn java_shuffle<T>(v: &mut [T], rng: &mut JavaRandom) {
    for i in (1..v.len()).rev() {
        let j = rng.next_int_bound(i as i32 + 1) as usize;
        v.swap(i, j);
    }
}

/// `CardGroup.shuffle()` 语义：从 STS 流取一个 long → 种子化 java.util.Random → 洗牌
pub fn card_group_shuffle<T>(v: &mut [T], stream: &mut StsRandom) {
    let mut jr = JavaRandom::new(stream.random_long());
    java_shuffle(v, &mut jr);
}

/// 游戏的 13 条 RNG 流（AbstractDungeon 481-492 行：全部同 `Settings.seed` 初始化）
#[derive(Clone, Debug)]
pub struct RngStreams {
    pub monster: StsRandom,
    pub map: StsRandom,
    pub event: StsRandom,
    pub merchant: StsRandom,
    pub card: StsRandom,
    pub treasure: StsRandom,
    pub relic: StsRandom,
    pub potion: StsRandom,
    pub monster_hp: StsRandom,
    pub ai: StsRandom,
    pub shuffle: StsRandom,
    pub card_random: StsRandom,
    pub misc: StsRandom,
}

impl RngStreams {
    pub fn new(seed: u64) -> Self {
        Self {
            monster: StsRandom::new(seed),
            map: StsRandom::new(seed),
            event: StsRandom::new(seed),
            merchant: StsRandom::new(seed),
            card: StsRandom::new(seed),
            treasure: StsRandom::new(seed),
            relic: StsRandom::new(seed),
            potion: StsRandom::new(seed),
            monster_hp: StsRandom::new(seed),
            ai: StsRandom::new(seed),
            shuffle: StsRandom::new(seed),
            card_random: StsRandom::new(seed),
            misc: StsRandom::new(seed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../tests/golden_rng.txt");

    fn sec_i64(tag: &str) -> Vec<i64> {
        GOLDEN
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                (it.next() == Some(tag)).then(|| it.next().unwrap().parse::<i64>().unwrap())
            })
            .collect()
    }

    #[test]
    fn xs128_matches_game() {
        let mut xs = Xs128::new(12345);
        for want in sec_i64("xs.nextLong") {
            assert_eq!(xs.next_long() as i64, want);
        }
        for want in sec_i64("xs.nextInt52") {
            assert_eq!(xs.next_int_n(52) as i64, want);
        }
        for want in sec_i64("xs.nextBoolean") {
            assert_eq!(xs.next_boolean() as i64, want);
        }
        for want in sec_i64("xs.nextFloatBits") {
            assert_eq!(f32::to_bits(xs.next_float()) as i64, want);
        }
        for want in sec_i64("xs.nextDoubleBits") {
            assert_eq!(f64::to_bits(xs.next_double()) as i64, want);
        }
        for want in sec_i64("xs.nextInt100") {
            assert_eq!(xs.next_int_n(100) as i64, want);
        }
    }

    #[test]
    fn java_random_matches_jdk() {
        let mut jr = JavaRandom::new(987654321012345);
        for want in sec_i64("jr.nextInt52") {
            assert_eq!(jr.next_int_bound(52) as i64, want);
        }
        for want in sec_i64("jr.nextLong") {
            assert_eq!(jr.next_long(), want);
        }
        for want in sec_i64("jr.nextBoolean") {
            assert_eq!(jr.next_boolean() as i64, want);
        }
        for want in sec_i64("jr.nextFloatBits") {
            assert_eq!(f32::to_bits(jr.next_float()) as i64, want);
        }
        for want in sec_i64("jr.nextDoubleBits") {
            assert_eq!(f64::to_bits(jr.next_double()) as i64, want);
        }
    }

    #[test]
    fn java_shuffle_matches_collections_shuffle() {
        let line = GOLDEN.lines().find(|l| l.starts_with("shuffle52")).unwrap();
        let want: Vec<usize> = line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .split(',')
            .map(|s| s.parse().unwrap())
            .collect();
        let mut v: Vec<usize> = (0..52).collect();
        let mut jr = JavaRandom::new(42);
        java_shuffle(&mut v, &mut jr);
        assert_eq!(v, want);
    }

    #[test]
    fn sts_wrapper_matches_game() {
        let mut sts = StsRandom::new(12345);
        for want in sec_i64("sts.random99") {
            assert_eq!(sts.random_range(99) as i64, want);
        }
        for want in sec_i64("sts.random40_44") {
            assert_eq!(sts.random_start_end(40, 44) as i64, want);
        }
        for want in sec_i64("sts.randomBoolean") {
            assert_eq!(sts.random_boolean() as i64, want);
        }
        for want in sec_i64("sts.randomBoolean5625") {
            assert_eq!(sts.random_boolean_chance(0.5625) as i64, want);
        }
        for want in sec_i64("sts.randomLong") {
            assert_eq!(sts.random_long(), want);
        }
        for want in sec_i64("sts.randomFloatBits") {
            assert_eq!(f32::to_bits(sts.random_float()) as i64, want);
        }
        let counter = sec_i64("sts.counter")[0] as u32;
        assert_eq!(sts.counter, counter); // 88 = 20+20+16+16+8+8
    }

    #[test]
    fn card_group_shuffle_pipeline() {
        // 组合语义：一次洗牌恰好消耗 shuffle 流 1 个 counter（randomLong）
        let mut stream = StsRandom::new(7);
        let mut v: Vec<usize> = (0..10).collect();
        card_group_shuffle(&mut v, &mut stream);
        assert_eq!(stream.counter, 1);
        // 同流同序确定
        let mut stream2 = StsRandom::new(7);
        let mut v2: Vec<usize> = (0..10).collect();
        card_group_shuffle(&mut v2, &mut stream2);
        assert_eq!(v, v2);
    }

    #[test]
    fn streams_snapshot_restore_roundtrip() {
        let mut s = StsRandom::new(42);
        for _ in 0..10 {
            s.random_range(99);
        }
        let snap = s.snapshot();
        let a = s.random_long();
        let mut t = StsRandom::restore(snap.0, snap.1, snap.2);
        assert_eq!(t.random_long(), a);
    }
}
