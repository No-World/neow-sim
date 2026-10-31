//! 战斗状态结构。

use crate::cards::CardInst;
use crate::potions::PotionInst;
use crate::powers::Power;
use crate::rng::StsRandom;

#[derive(Clone, Debug)]
pub struct Creature {
    pub hp: i32,
    pub max_hp: i32,
    pub block: i32,
    /// 插入序 = Java ArrayList 序（浮点乘区顺序依赖此序）
    pub powers: Vec<Power>,
}

impl Creature {
    pub fn new(hp: i32, max_hp: i32) -> Self {
        Self {
            hp,
            max_hp,
            block: 0,
            powers: Vec::new(),
        }
    }

    /// decrementBlock + 扣血的 M2 简化（无 onAttacked 改伤类 power）
    pub fn apply_damage(&mut self, dmg: i32) {
        let mut d = dmg.max(0);
        if d > self.block {
            d -= self.block;
            self.block = 0;
        } else {
            self.block -= d;
            d = 0;
        }
        self.hp -= d;
    }

    pub fn is_dead(&self) -> bool {
        self.hp <= 0
    }
}

/// 观者姿态（stances/*.java：倍率只作用于 NORMAL）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stance {
    Neutral,
    Wrath,
    Calm,
    Divinity,
}

impl Stance {
    pub fn from_id(id: &str) -> Option<Stance> {
        match id {
            "Neutral" => Some(Stance::Neutral),
            "Wrath" => Some(Stance::Wrath),
            "Calm" => Some(Stance::Calm),
            "Divinity" => Some(Stance::Divinity),
            _ => None,
        }
    }

    /// WrathStance: atDamageGive ×2（NORMAL）
    pub fn at_damage_give(self, dmg: f32, ty: crate::powers::DamageType) -> f32 {
        match (self, ty) {
            (Stance::Wrath, crate::powers::DamageType::Normal) => dmg * 2.0,
            (Stance::Divinity, crate::powers::DamageType::Normal) => dmg * 3.0,
            _ => dmg,
        }
    }

    /// WrathStance: atDamageReceive ×2（NORMAL）
    pub fn at_damage_receive(self, dmg: f32, ty: crate::powers::DamageType) -> f32 {
        match (self, ty) {
            (Stance::Wrath, crate::powers::DamageType::Normal) => dmg * 2.0,
            _ => dmg,
        }
    }
}

/// 充能球种类（orbs/*.java）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrbKind {
    Frost,
    Lightning,
    Dark,
    Plasma,
}

impl OrbKind {
    pub fn from_id(id: &str) -> Option<OrbKind> {
        match id {
            "Frost" => Some(OrbKind::Frost),
            "Lightning" => Some(OrbKind::Lightning),
            "Dark" => Some(OrbKind::Dark),
            "Plasma" => Some(OrbKind::Plasma),
            _ => None,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            OrbKind::Frost => "Frost",
            OrbKind::Lightning => "Lightning",
            OrbKind::Dark => "Dark",
            OrbKind::Plasma => "Plasma",
        }
    }

    /// 基础值（orbs/*.java 构造器）
    pub fn base_passive(&self) -> i32 {
        match self {
            OrbKind::Frost => 2,
            OrbKind::Lightning => 3,
            OrbKind::Dark => 6,
            OrbKind::Plasma => 1,
        }
    }

    pub fn base_evoke(&self) -> i32 {
        match self {
            OrbKind::Frost => 5,
            OrbKind::Lightning => 8,
            OrbKind::Dark => 6,
            OrbKind::Plasma => 2,
        }
    }
}

/// 单个充能球（Dark 的 evoke 会通过 EOT 被动自增长）
#[derive(Clone, Debug)]
pub struct OrbState {
    pub kind: OrbKind,
    /// Dark 专属：EOT evoke_amount += passive_amount 的累积器
    pub dark_growth: i32,
}

impl OrbState {
    pub fn new(kind: OrbKind) -> Self {
        Self {
            kind,
            dark_growth: 0,
        }
    }

    /// 有效被动值（applyFocus：base + 专注，下限 0；Plasma 不吃专注）
    pub fn passive(&self, focus: i32) -> i32 {
        match self.kind {
            OrbKind::Plasma => self.kind.base_passive(),
            _ => (self.kind.base_passive() + focus).max(0),
        }
    }

    /// 有效引出值（base + 专注 + Dark 自增长；Plasma 不吃专注）
    pub fn evoke(&self, focus: i32) -> i32 {
        match self.kind {
            OrbKind::Plasma => self.kind.base_evoke(),
            _ => (self.kind.base_evoke() + focus + self.dark_growth).max(0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlayerState {
    pub creature: Creature,
    pub stance: Stance,
    pub energy: i32,
    /// 充能球槽（左→右，只记已填的；空槽数 = max_orbs - orbs.len()）
    pub orbs: Vec<OrbState>,
    pub max_orbs: i32,
    pub hand: Vec<CardInst>,
    /// Vec 尾 = 抽牌堆顶（CardGroup.group 末位）
    pub draw: Vec<CardInst>,
    pub discard: Vec<CardInst>,
    pub exhaust: Vec<CardInst>,
    /// 药水栏（AbstractPlayer.potionSlots = 3；None = 空栏）
    pub potions: Vec<Option<PotionInst>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Attack,
    DefendBuff,
    AttackDefend,
    Buff,
    Debuff,
    AttackDebuff,
    Defend,
    Escape,
    Sleep,
    Unknown,
    AttackBuff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MonsterKind {
    JawWorm,
    Cultist,
    LouseNormal,
    LouseDefensive,
    FungiBeast,
    SpikeSlimeS,
    AcidSlimeS,
    SpikeSlimeM,
    AcidSlimeM,
    Looter,
    SlaverBlue,
    SlaverRed,
    GremlinNob,
    Lagavulin,
    Sentry,
    Guardian,
    SlimeBoss,
    Hexaghost,
    // —— Act 2（TheCity）普通遭遇 ——
    SphericGuardian,
    Chosen,
    ShelledParasite,
    Byrd,
    Mugger,
    SnakePlant,
    Snecko,
    Centurion,
    Healer,
    // —— Act 2 精英/Boss 及召唤物 ——
    GremlinWarrior,
    GremlinThief,
    GremlinFat,
    GremlinTsundere,
    GremlinWizard,
    GremlinLeader,
    BookOfStabbing,
    Taskmaster,
    BronzeOrb,
    BronzeAutomaton,
    TorchHead,
    TheCollector,
    Champ,
    // —— Act 3（TheBeyond）普通遭遇 ——
    Darkling,
    Repulsor,
    OrbWalker,
    SpireGrowth,
    Transient,
    Maw,
    WrithingMass,
    // —— Act 3 精英/Boss 及召唤物 ——
    GiantHead,
    Nemesis,
    Reptomancer,
    SnakeDagger,
    AwakenedOne,
    Donu,
    Deca,
    TimeEater,
}

/// 通用怪物状态（M2 覆盖：大颚虫、邪教徒；M3 走数据驱动定义表）
#[derive(Clone, Debug)]
pub struct MonsterState {
    pub kind: MonsterKind,
    pub creature: Creature,
    pub next_move: u8,
    pub move_history: Vec<u8>,
    pub first_move: bool,
    // 大颚虫
    pub chomp_dmg: i32,
    pub thrash_dmg: i32,
    pub thrash_block: i32,
    pub bellow_str: i32,
    pub bellow_block: i32,
    // 邪教徒
    pub dark_strike_dmg: i32,
    pub ritual_amt: i32,
    /// 通用计数器：Hexaghost=灼伤升级 / Mugger=已偷金币 / BookOfStabbing=连击数 /
    /// Wizard=已充能 / Automaton=回合计数 等
    pub burn_upgraded: i32,
    /// Mugger/Looter 逃离（不计死亡，不触发亡语；全员逃离=战斗结束）
    pub escaped: bool,
    /// 铜球静滞：吸走的那张玩家卡（死亡时归还手牌）
    pub stasis_card: Option<CardInst>,
    /// 小暗灵半死状态（全员半死=真死；否则下回合复活半血）
    pub half_dead: bool,
}

impl MonsterState {
    /// 构造：按种类掷 HP（monsterHpRng 流，调用方传入保证流序）
    pub fn spawn(kind: MonsterKind, ascension: i32, hp_rng: &mut StsRandom) -> Self {
        let (hp, fields) = match kind {
            MonsterKind::JawWorm => {
                let (chomp, td, tb, bs, bb, (lo, hi)) = Self::jaw_worm_stats(ascension);
                (
                    hp_rng.random_start_end(lo, hi),
                    (chomp, td, tb, bs, bb, 6, 3),
                )
            }
            MonsterKind::Cultist => {
                let (lo, hi) = if ascension >= 7 { (50, 56) } else { (48, 54) };
                let ritual = if ascension >= 17 {
                    (if ascension >= 2 { 4 } else { 3 }) + 1
                } else if ascension >= 2 {
                    4
                } else {
                    3
                };
                (hp_rng.random_start_end(lo, hi), (0, 0, 0, 0, 0, 6, ritual))
            }
            MonsterKind::LouseNormal => {
                let (lo, hi) = if ascension >= 7 { (11, 16) } else { (10, 15) };
                let bite = if ascension >= 2 {
                    hp_rng.random_start_end(6, 8)
                } else {
                    hp_rng.random_start_end(5, 7)
                };
                (hp_rng.random_start_end(lo, hi), (bite, 3, 0, 0, 0, 0, 0))
            }
            MonsterKind::FungiBeast => {
                let (lo, hi) = if ascension >= 7 { (24, 28) } else { (22, 28) };
                let str_amt = if ascension >= 2 { 4 } else { 3 };
                (hp_rng.random_start_end(lo, hi), (6, str_amt, 0, 0, 0, 0, 0))
            }
            MonsterKind::LouseDefensive => {
                let (lo, hi) = if ascension >= 7 { (12, 18) } else { (11, 17) };
                let bite = if ascension >= 2 {
                    hp_rng.random_start_end(6, 8)
                } else {
                    hp_rng.random_start_end(5, 7)
                };
                (hp_rng.random_start_end(lo, hi), (bite, 2, 0, 0, 0, 0, 0))
            }
            MonsterKind::SpikeSlimeS => {
                let (lo, hi) = if ascension >= 7 { (11, 15) } else { (10, 14) };
                let dmg = if ascension >= 2 { 6 } else { 5 };
                (hp_rng.random_start_end(lo, hi), (dmg, 0, 0, 0, 0, 0, 0))
            }
            MonsterKind::AcidSlimeS => {
                let (lo, hi) = if ascension >= 7 { (9, 13) } else { (8, 12) };
                let dmg = if ascension >= 2 { 4 } else { 3 };
                (hp_rng.random_start_end(lo, hi), (dmg, 1, 0, 0, 0, 0, 0))
            }
            MonsterKind::SpikeSlimeM => {
                let (lo, hi) = if ascension >= 7 { (29, 34) } else { (28, 32) };
                let dmg = if ascension >= 2 { 10 } else { 8 };
                (hp_rng.random_start_end(lo, hi), (dmg, 1, 0, 0, 0, 0, 0))
            }
            MonsterKind::AcidSlimeM => {
                let (lo, hi) = if ascension >= 7 { (29, 34) } else { (28, 32) };
                let dmg = if ascension >= 2 { 8 } else { 7 };
                let dmg2 = if ascension >= 2 { 11 } else { 10 };
                (hp_rng.random_start_end(lo, hi), (dmg, 1, dmg2, 0, 0, 0, 0))
            }
            MonsterKind::Looter => {
                let (lo, hi) = if ascension >= 7 { (46, 50) } else { (44, 48) };
                (hp_rng.random_start_end(lo, hi), (10, 15, 12, 0, 0, 0, 0))
            }
            MonsterKind::SlaverBlue => {
                let (lo, hi) = if ascension >= 7 { (48, 52) } else { (46, 50) };
                let stab = if ascension >= 2 { 13 } else { 12 };
                let rake = if ascension >= 2 { 8 } else { 7 };
                (hp_rng.random_start_end(lo, hi), (stab, 1, rake, 0, 0, 0, 0))
            }
            MonsterKind::SlaverRed => {
                let (lo, hi) = if ascension >= 7 { (48, 52) } else { (46, 50) };
                let stab = if ascension >= 2 { 14 } else { 13 };
                let scrape = if ascension >= 2 { 9 } else { 8 };
                (
                    hp_rng.random_start_end(lo, hi),
                    (stab, 1, scrape, 0, 0, 0, 0),
                )
            }
            MonsterKind::GremlinNob => {
                let (lo, hi) = if ascension >= 2 { (85, 90) } else { (82, 86) };
                let bash = if ascension >= 2 { 8 } else { 6 };
                let rush = if ascension >= 2 { 16 } else { 14 };
                (hp_rng.random_start_end(lo, hi), (rush, 2, bash, 0, 2, 0, 0))
            }
            MonsterKind::Lagavulin => {
                let (lo, hi) = if ascension >= 2 {
                    (112, 115)
                } else {
                    (109, 111)
                };
                let dmg = if ascension >= 3 { 20 } else { 18 };
                let debuff = if ascension >= 18 { 2 } else { 1 };
                (
                    hp_rng.random_start_end(lo, hi),
                    (dmg, debuff, 0, 8, 0, 0, 0),
                )
            }
            MonsterKind::Sentry => {
                let (lo, hi) = if ascension >= 2 { (39, 45) } else { (38, 42) };
                let beam = if ascension >= 3 { 10 } else { 9 };
                (hp_rng.random_start_end(lo, hi), (beam, 2, 0, 0, 0, 0, 0))
            }
            MonsterKind::Guardian => {
                let hp = if ascension >= 2 { 250 } else { 240 };
                let bash = if ascension >= 2 { 36 } else { 32 };
                let roll = if ascension >= 2 { 10 } else { 9 };
                (hp, (bash, roll, 5, 8, 9, 2, 4))
            }
            MonsterKind::SlimeBoss => {
                let hp = if ascension >= 2 { 150 } else { 140 };
                let slam = if ascension >= 2 { 38 } else { 35 };
                (hp, (slam, 10, 3, 0, 0, 0, 0))
            }
            // 六火亡魂：字段映射 —— chomp=火焰冲撞/撞，thrash=地狱火(每段)，
            // thrash_block=煽燃力量，bellow_str=灼烙灼伤数，bellow_block=分割者段伤
            //（激活回合锁定=玩家 HP/12+1），dark_strike=灼烙伤 6，ritual=已激活球数
            MonsterKind::Hexaghost => {
                // Boss 血量提升在 A9（构造器：>=9 → 264，否则 250）
                let hp = if ascension >= 9 { 264 } else { 250 };
                if ascension >= 19 {
                    (hp, (6, 3, 3, 2, 0, 6, 0))
                } else if ascension >= 4 {
                    (hp, (6, 3, 2, 1, 0, 6, 0))
                } else {
                    (hp, (5, 2, 2, 1, 0, 6, 0))
                }
            }
            // 球形守护者：HP 固定 20；开局壁垒+神器3+40格挡（战斗外初始化见 new_with）
            // 字段：chomp=伤害(10/11) thrash=充能格挡(25/35) thrash_block=硬化(15)
            // bellow_str=脆弱(5)
            MonsterKind::SphericGuardian => (
                20,
                (
                    if ascension >= 2 { 11 } else { 10 },
                    if ascension >= 17 { 35 } else { 25 },
                    15,
                    5,
                    0,
                    0,
                    0,
                ),
            ),
            // 天选者：chomp=电击(18/21) thrash=衰弱(10/12) thrash_block=戳刺(5/6)
            // bellow_str=汲取力量(3) bellow_block=汲取虚弱(3) dark=易伤(2) ritual=咒缚(1)
            MonsterKind::Chosen => {
                let (lo, hi) = if ascension >= 7 { (98, 103) } else { (95, 99) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 21 } else { 18 },
                        if ascension >= 2 { 12 } else { 10 },
                        if ascension >= 2 { 6 } else { 5 },
                        3,
                        3,
                        2,
                        1,
                    ),
                )
            }
            // 灼壳寄生虫：开局多层护甲14+14格挡；chomp=猛击(18/21)
            // thrash=双重打击(6/7) thrash_block=吸取(10/12) bellow_str=脆弱(2)
            MonsterKind::ShelledParasite => {
                let (lo, hi) = if ascension >= 7 { (70, 75) } else { (68, 72) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 21 } else { 18 },
                        if ascension >= 2 { 7 } else { 6 },
                        if ascension >= 2 { 12 } else { 10 },
                        2,
                        0,
                        0,
                        0,
                    ),
                )
            }
            // 伯德鸟：chomp=啄(1) thrash=俯冲(12/14) thrash_block=头槌(3)
            // bellow_str=飞行层数(3/4) bellow_block=啄次数(5/6) ritual=鸣叫力量(1)
            MonsterKind::Byrd => {
                let (lo, hi) = if ascension >= 7 { (26, 33) } else { (25, 31) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        1,
                        if ascension >= 2 { 14 } else { 12 },
                        3,
                        if ascension >= 17 { 4 } else { 3 },
                        if ascension >= 2 { 6 } else { 5 },
                        0,
                        1,
                    ),
                )
            }
            // 劫匪：chomp=抢劫(10/11) thrash=重扫(16/18) thrash_block=烟雾(11)
            // bellow_str=偷金(15/20)；A17 烟雾格挡+6
            MonsterKind::Mugger => {
                let (lo, hi) = if ascension >= 7 { (50, 54) } else { (48, 52) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 11 } else { 10 },
                        if ascension >= 2 { 18 } else { 16 },
                        11,
                        if ascension >= 17 { 20 } else { 15 },
                        0,
                        0,
                        0,
                    ),
                )
            }
            // 咬人蛇草：chomp=撕咬(7/8)×3 bellow_str=孢子脆弱(2) bellow_block=孢子虚弱(2)
            MonsterKind::SnakePlant => {
                let (lo, hi) = if ascension >= 7 { (78, 82) } else { (75, 79) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 8 } else { 7 }, 3, 0, 2, 2, 0, 0),
                )
            }
            // 斯涅克：chomp=咬(15/18) thrash=尾击(8/10) bellow_str=易伤/虚弱(2)
            MonsterKind::Snecko => {
                let (lo, hi) = if ascension >= 7 {
                    (120, 125)
                } else {
                    (114, 120)
                };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 18 } else { 15 },
                        if ascension >= 2 { 10 } else { 8 },
                        0,
                        2,
                        0,
                        0,
                        0,
                    ),
                )
            }
            // 百夫长：chomp=斩击(12/14) thrash=狂怒(6/7)×3 bellow_str=段数(3)
            // bellow_block=护卫格挡(15/20)
            MonsterKind::Centurion => {
                let (lo, hi) = if ascension >= 7 { (78, 83) } else { (76, 80) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 14 } else { 12 },
                        if ascension >= 2 { 7 } else { 6 },
                        3,
                        0,
                        if ascension >= 17 { 20 } else { 15 },
                        0,
                        0,
                    ),
                )
            }
            // 拜蛇教祭司：chomp=魔法(8/9) thrash=治疗(16) bellow_str=力量(2/3)
            // bellow_block=脆弱(2)
            MonsterKind::Healer => {
                let (lo, hi) = if ascension >= 7 { (50, 58) } else { (48, 56) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 9 } else { 8 },
                        16,
                        if ascension >= 17 { 3 } else { 2 },
                        2,
                        0,
                        0,
                        0,
                    ),
                )
            }
            // ════════ Act 2 精英/Boss 及召唤物 ════════
            // 战鬼：chomp=抓击(4/5)；战前 Angry 1(2 A17)
            MonsterKind::GremlinWarrior => {
                let (lo, hi) = if ascension >= 7 { (21, 25) } else { (20, 24) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 5 } else { 4 }, 0, 0, 0, 0, 0, 0),
                )
            }
            // 暴徒：chomp=穿刺(9/10)
            MonsterKind::GremlinThief => {
                let (lo, hi) = if ascension >= 7 { (11, 15) } else { (10, 14) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 10 } else { 9 }, 0, 0, 0, 0, 0, 0),
                )
            }
            // 胖鬼：chomp=钝击(4/5)+弱1(A17 再+脆1)：thrash=弱1 thrash_block=脆1
            MonsterKind::GremlinFat => {
                let (lo, hi) = if ascension >= 7 { (14, 18) } else { (13, 17) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 5 } else { 4 }, 1, 1, 0, 0, 0, 0),
                )
            }
            // 傲鬼：chomp=痛击(6/8) thrash=保护格挡(7/8/11 A17)
            MonsterKind::GremlinTsundere => {
                let (lo, hi) = if ascension >= 7 { (13, 17) } else { (12, 15) };
                let block = if ascension >= 17 {
                    11
                } else if ascension >= 7 {
                    8
                } else {
                    7
                };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 8 } else { 6 }, block, 0, 0, 0, 0, 0),
                )
            }
            // 巫鬼：chomp=魔法(25/30)；counter=currentCharge 起 1
            MonsterKind::GremlinWizard => {
                let (lo, hi) = if ascension >= 7 { (22, 26) } else { (21, 25) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 30 } else { 25 }, 0, 0, 0, 0, 0, 0),
                )
            }
            // 小鬼首领：chomp=刺击6×3(bellow_str=段数3) thrash=激励力量(3/4/5)
            // thrash_block=激励格挡(6/6/10)
            MonsterKind::GremlinLeader => {
                let (lo, hi) = if ascension >= 7 {
                    (145, 155)
                } else {
                    (140, 148)
                };
                let (str_amt, block_amt) = if ascension >= 17 {
                    (5, 10)
                } else if ascension >= 2 {
                    (4, 6)
                } else {
                    (3, 6)
                };
                (
                    hp_rng.random_start_end(lo, hi),
                    (6, str_amt, block_amt, 3, 0, 0, 0),
                )
            }
            // 连击之书：chomp=刺(6/7) thrash=重刺(21/24)；counter=stabCount 起 1
            MonsterKind::BookOfStabbing => {
                let (lo, hi) = if ascension >= 8 {
                    (168, 172)
                } else {
                    (160, 164)
                };
                let (stab, big) = if ascension >= 3 { (7, 24) } else { (6, 21) };
                (hp_rng.random_start_end(lo, hi), (stab, big, 0, 0, 0, 0, 0))
            }
            // 监工：chomp=鞭笞7 + thrash=伤口数(1/2 A3/3 A18)（A18 再自+1 力量）
            MonsterKind::Taskmaster => {
                let (lo, hi) = if ascension >= 8 { (57, 64) } else { (54, 60) };
                let wounds = if ascension >= 18 {
                    3
                } else if ascension >= 3 {
                    2
                } else {
                    1
                };
                (hp_rng.random_start_end(lo, hi), (7, wounds, 0, 0, 0, 0, 0))
            }
            // 铜球：chomp=光束8 thrash=支援格挡12；STASIS 吸牌（死亡归还）
            MonsterKind::BronzeOrb => {
                let (lo, hi) = if ascension >= 9 { (54, 60) } else { (52, 58) };
                (hp_rng.random_start_end(lo, hi), (8, 12, 0, 0, 0, 0, 0))
            }
            // 铜制自动机：chomp=连枷(7/8)×2 thrash=超究光束(45/50) thrash_block=强化格挡(9/12)
            // bellow_str=强化力量(3/4)；counter=回合计；战前神器3
            MonsterKind::BronzeAutomaton => {
                let hp = if ascension >= 9 { 320 } else { 300 };
                let (flail, beam, str_amt) = if ascension >= 4 {
                    (8, 50, 4)
                } else {
                    (7, 45, 3)
                };
                let block_amt = if ascension >= 9 { 12 } else { 9 };
                (hp, (flail, beam, block_amt, str_amt, 0, 0, 0))
            }
            // 火把头：chomp=冲撞7（恒定，SetMove 驱动）
            MonsterKind::TorchHead => {
                let (lo, hi) = if ascension >= 9 { (40, 45) } else { (38, 40) };
                (hp_rng.random_start_end(lo, hi), (7, 0, 0, 0, 0, 0, 0))
            }
            // 收集者：chomp=火球(18/21) thrash=群力(3/4/5) thrash_block=自我格挡(15/18)
            // bellow_str=超大减益(3/5 A19)；turns=history.len()
            MonsterKind::TheCollector => {
                let hp = if ascension >= 9 { 300 } else { 282 };
                let (fb, str_amt, mega) = if ascension >= 19 {
                    (21, 5, 5)
                } else if ascension >= 4 {
                    (21, 4, 3)
                } else {
                    (18, 3, 3)
                };
                let block_amt = if ascension >= 9 { 18 } else { 15 };
                (hp, (fb, str_amt, block_amt, mega, 0, 0, 0))
            }
            // 冠军：chomp=重斩(16/18) thrash=处决10×2 thrash_block=扇击(12/14)
            // bellow_str=力量(2/3/4) bellow_block=锻造金属化(5/6/7) dark_strike=格挡(15/18/20)
            MonsterKind::Champ => {
                let hp = if ascension >= 9 { 440 } else { 420 };
                let slash = if ascension >= 4 { 18 } else { 16 };
                let slap = if ascension >= 4 { 14 } else { 12 };
                let (str_amt, forge, block_amt) = if ascension >= 19 {
                    (4, 7, 20)
                } else if ascension >= 9 {
                    (3, 6, 18)
                } else if ascension >= 4 {
                    (3, 5, 15)
                } else {
                    (2, 5, 15)
                };
                (hp, (slash, 10, slap, str_amt, forge, block_amt, 0))
            }
            // ════════ Act 3（TheBeyond）普通遭遇 ══════════
            // 小暗灵：chomp=撕咬(8/9)×2 thrash=噬咬(random 7-11/9-13) thrash_block=硬化12
            // bellow_str=A17 硬化附力量2；halfDead 复活机制见 combat
            MonsterKind::Darkling => {
                let (lo, hi) = if ascension >= 7 { (50, 59) } else { (48, 56) };
                let nip = if ascension >= 2 {
                    hp_rng.random_start_end(9, 13)
                } else {
                    hp_rng.random_start_end(7, 11)
                };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 9 } else { 8 }, nip, 12, 2, 0, 0, 0),
                )
            }
            // 排斥体：chomp=攻击(11/13) thrash=晕眩卡数2
            MonsterKind::Repulsor => {
                let (lo, hi) = if ascension >= 7 { (31, 38) } else { (29, 35) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (if ascension >= 2 { 13 } else { 11 }, 2, 0, 0, 0, 0, 0),
                )
            }
            // 天球行者：chomp=激光(10/11) thrash=利爪(15/16)；战前力量 3(5 A17)
            MonsterKind::OrbWalker => {
                let (lo, hi) = if ascension >= 7 { (92, 102) } else { (90, 96) };
                (
                    hp_rng.random_start_end(lo, hi),
                    (
                        if ascension >= 2 { 11 } else { 10 },
                        if ascension >= 2 { 16 } else { 15 },
                        0,
                        0,
                        0,
                        0,
                        0,
                    ),
                )
            }
            // 尖塔生长体：chomp=快击(16/18) thrash=重击(22/25) thrash_block=紧缠(10)
            MonsterKind::SpireGrowth => {
                let hp = if ascension >= 7 { 190 } else { 170 };
                let (tackle, smash) = if ascension >= 2 { (18, 25) } else { (16, 22) };
                (hp, (tackle, smash, 10, 0, 0, 0, 0))
            }
            // 转瞬即逝：chomp=起始死兆(30/40) thrash=每回合增量10；counter=count；
            // 战前 Fading 5(6 A17) + Shifting
            MonsterKind::Transient => (
                999,
                (if ascension >= 2 { 40 } else { 30 }, 10, 0, 0, 0, 0, 0),
            ),
            // 巨口：chomp=猛击(25/30) thrash=啃食5 thrash_block=咆哮时长3；
            // bellow_str=流涎力量3；counter=turnCount 起 1
            MonsterKind::Maw => (
                300,
                (if ascension >= 2 { 30 } else { 25 }, 5, 3, 3, 0, 0, 0),
            ),
            // 蠕动聚块：chomp=巨击(32/38) thrash=连击(7/9)×3 thrash_block=攻防(15/16)
            // bellow_str=削弱攻击(10/12) bellow_block=弱易2；战前 Reactive+Malleable
            MonsterKind::WrithingMass => {
                let hp = if ascension >= 7 { 175 } else { 160 };
                if ascension >= 2 {
                    (hp, (38, 9, 16, 12, 2, 0, 0))
                } else {
                    (hp, (32, 7, 15, 10, 2, 0, 0))
                }
            }
            // ════════ Act 3 精英/Boss 及召唤物 ══════════
            // 巨首：chomp=计数打击13 thrash=死兆(30/40 A3) thrash_block=每层+5；
            // counter=count 起 5；战前自身 Slow
            MonsterKind::GiantHead => {
                let hp = if ascension >= 8 { 520 } else { 500 };
                (
                    hp,
                    (13, if ascension >= 3 { 40 } else { 30 }, 5, 0, 0, 0, 0),
                )
            }
            // 复仇者：chomp=镰刀45 thrash=三连火(6/7 A3)×3 thrash_block=灼伤(3/5 A18)；
            // counter=镰刀冷却；每回合尾自动无形 1
            MonsterKind::Nemesis => {
                let hp = if ascension >= 8 { 200 } else { 185 };
                (
                    hp,
                    (
                        45,
                        if ascension >= 3 { 7 } else { 6 },
                        if ascension >= 18 { 5 } else { 3 },
                        0,
                        0,
                        0,
                        0,
                    ),
                )
            }
            // 爬虫法师：chomp=蛇击(13/16)×2+弱1 thrash=重咬(30/34)
            // thrash_block=每次召唤数(1/2 A18)；首招召唤；canSpawn≤3
            MonsterKind::Reptomancer => {
                let (lo, hi) = if ascension >= 8 {
                    (190, 200)
                } else {
                    (180, 190)
                };
                if ascension >= 3 {
                    (
                        hp_rng.random_start_end(lo, hi),
                        (16, 34, if ascension >= 18 { 2 } else { 1 }, 1, 0, 0, 0),
                    )
                } else {
                    (
                        hp_rng.random_start_end(lo, hi),
                        (13, 30, if ascension >= 18 { 2 } else { 1 }, 1, 0, 0, 0),
                    )
                }
            }
            // 蛇匕：chomp=刺9 thrash=自爆25（打完自灭）
            MonsterKind::SnakeDagger => {
                let (lo, hi) = (20, 25);
                (hp_rng.random_start_end(lo, hi), (9, 25, 0, 0, 0, 0, 0))
            }
            // 觉醒者：双阶段(300/300, A9 320)；chomp=挥砍20 thrash=灵魂冲击6×4
            // thrash_block=黑暗回响40 bellow_str=污泥18 bellow_block=冲撞10
            // dark=冲击段数4 ritual=冲撞段数3；counter=阶段2标记
            MonsterKind::AwakenedOne => {
                let hp = if ascension >= 9 { 320 } else { 300 };
                (hp, (20, 6, 40, 18, 10, 4, 3))
            }
            // 多努：chomp=光束(10/12)×2 thrash=力量环3；起手力量环；神器2(3 A19)
            MonsterKind::Donu => {
                let hp = if ascension >= 9 { 265 } else { 250 };
                (hp, (if ascension >= 4 { 12 } else { 10 }, 3, 0, 0, 0, 0, 0))
            }
            // 德卡：chomp=光束(10/12)×2+晕2 thrash=护甲环3；起手光束；神器2(3 A19)
            MonsterKind::Deca => {
                let hp = if ascension >= 9 { 265 } else { 250 };
                (hp, (if ascension >= 4 { 12 } else { 10 }, 3, 2, 0, 0, 0, 0))
            }
            // 吞时者：chomp=回响(7/8)×3 thrash=头撞(26/32)+下回合抽牌-1
            // thrash_block=涟漪格挡20；半血一次加速；战前 TimeWarp
            MonsterKind::TimeEater => {
                let hp = if ascension >= 9 { 480 } else { 456 };
                let (reverb, slam) = if ascension >= 2 { (8, 32) } else { (7, 26) };
                (hp, (reverb, slam, 20, 0, 0, 0, 0))
            }
        };
        Self {
            kind,
            creature: Creature::new(hp, hp),
            next_move: 1,
            move_history: Vec::new(),
            first_move: true,
            chomp_dmg: fields.0,
            thrash_dmg: fields.1,
            thrash_block: fields.2,
            bellow_str: fields.3,
            bellow_block: fields.4,
            dark_strike_dmg: fields.5,
            ritual_amt: fields.6,
            // 通用计数器：Hexaghost=灼伤升级标志 / Mugger=已偷金币；其余怪构 0
            burn_upgraded: 0,
            escaped: false,
            stasis_card: None,
            half_dead: false,
        }
    }

    /// (chomp, thrashDmg, thrashBlock, bellowStr, bellowBlock, hpRange)
    fn jaw_worm_stats(ascension: i32) -> (i32, i32, i32, i32, i32, (i32, i32)) {
        let hp = if ascension >= 7 { (42, 46) } else { (40, 44) };
        if ascension >= 17 {
            (12, 7, 5, 5, 9, hp)
        } else if ascension >= 2 {
            (12, 7, 5, 4, 6, hp)
        } else {
            (11, 7, 5, 3, 6, hp)
        }
    }

    pub fn intent(&self) -> Intent {
        match self.kind {
            MonsterKind::JawWorm => match self.next_move {
                1 => Intent::Attack,
                2 => Intent::DefendBuff,
                3 => Intent::AttackDefend,
                _ => Intent::Attack,
            },
            MonsterKind::Cultist => match self.next_move {
                3 => Intent::Buff,   // 仪式
                _ => Intent::Attack, // 暗击
            },
            MonsterKind::LouseNormal => match self.next_move {
                3 => Intent::Attack, // 咬
                4 => Intent::Buff,   // 蜷缩增长
                _ => Intent::Attack,
            },
            MonsterKind::FungiBeast => match self.next_move {
                1 => Intent::Attack, // 咬
                2 => Intent::Buff,   // 生长
                _ => Intent::Attack,
            },
            MonsterKind::LouseDefensive => match self.next_move {
                3 => Intent::Attack,
                4 => Intent::Debuff,
                _ => Intent::Attack,
            },
            MonsterKind::SpikeSlimeS => Intent::Attack,
            MonsterKind::AcidSlimeS => match self.next_move {
                1 => Intent::Attack,
                _ => Intent::Debuff,
            },
            MonsterKind::SpikeSlimeM => match self.next_move {
                1 => Intent::AttackDefend,
                _ => Intent::Debuff,
            },
            MonsterKind::AcidSlimeM => match self.next_move {
                1 | 2 => Intent::Attack,
                _ => Intent::Debuff,
            },
            MonsterKind::Looter => match self.next_move {
                1 | 4 => Intent::Attack,
                2 => Intent::Defend,
                3 => Intent::Escape,
                _ => Intent::Attack,
            },
            MonsterKind::SlaverBlue => match self.next_move {
                1 => Intent::Attack,
                _ => Intent::AttackDebuff,
            },
            MonsterKind::SlaverRed => match self.next_move {
                1 => Intent::Attack,
                _ => Intent::AttackDebuff,
            },
            MonsterKind::GremlinNob => match self.next_move {
                1 => Intent::Attack,
                2 => Intent::AttackDebuff,
                3 => Intent::Buff,
                _ => Intent::Attack,
            },
            MonsterKind::Lagavulin => match self.next_move {
                1 => Intent::Debuff,
                3 => Intent::Attack,
                5 => Intent::Sleep,
                _ => Intent::Attack,
            },
            MonsterKind::Sentry => match self.next_move {
                3 => Intent::Debuff,
                4 => Intent::Attack,
                _ => Intent::Attack,
            },
            MonsterKind::Guardian => match self.next_move {
                2 => Intent::Attack,
                3 => Intent::Attack,
                4 => Intent::AttackBuff,
                5 => Intent::Attack,
                6 => Intent::Defend,
                7 => Intent::Debuff,
                _ => Intent::Defend,
            },
            MonsterKind::SlimeBoss => match self.next_move {
                1 => Intent::Attack,
                2 => Intent::Unknown,
                4 => Intent::Debuff,
                _ => Intent::Attack,
            },
            MonsterKind::Hexaghost => match self.next_move {
                5 => Intent::Unknown,          // Activate
                3 => Intent::DefendBuff,       // Inflame
                4 | 6 => Intent::AttackDebuff, // Sear / Inferno
                _ => Intent::Attack,           // Divider / Tackle
            },
            MonsterKind::SphericGuardian => match self.next_move {
                2 => Intent::Defend,
                3 => Intent::AttackDefend,
                4 => Intent::AttackDebuff,
                _ => Intent::Attack,
            },
            MonsterKind::Chosen => match self.next_move {
                2 | 4 => Intent::Debuff, // Drain / Hex（STRONG_DEBUFF 归 Debuff）
                3 => Intent::AttackDebuff,
                _ => Intent::Attack, // Zap / Poke
            },
            MonsterKind::ShelledParasite => match self.next_move {
                1 => Intent::AttackDebuff,
                2 => Intent::Attack,
                3 => Intent::AttackBuff, // 吸取（吸血）
                _ => Intent::Sleep,      // 破甲眩晕
            },
            MonsterKind::Byrd => match self.next_move {
                2 => Intent::Unknown, // Go Airborne
                4 => Intent::Sleep,   // 落地眩晕
                6 => Intent::Buff,    // Caw
                _ => Intent::Attack,  // Peck / Swoop / Headbutt
            },
            MonsterKind::Mugger => match self.next_move {
                2 => Intent::Defend,
                3 => Intent::Escape,
                _ => Intent::Attack, // Mug / Big Swipe
            },
            MonsterKind::SnakePlant => match self.next_move {
                2 => Intent::Debuff, // Spores
                _ => Intent::Attack, // Chomp ×3
            },
            MonsterKind::Snecko => match self.next_move {
                1 => Intent::Debuff, // Glare（混乱）
                3 => Intent::AttackDebuff,
                _ => Intent::Attack,
            },
            MonsterKind::Centurion => match self.next_move {
                2 => Intent::Defend, // Protect
                _ => Intent::Attack, // Slash / Fury
            },
            MonsterKind::Healer => match self.next_move {
                1 => Intent::AttackDebuff,
                2 | 3 => Intent::Buff, // Heal / Buff
                _ => Intent::Attack,
            },
            MonsterKind::GremlinWarrior | MonsterKind::GremlinThief | MonsterKind::TorchHead => {
                match self.next_move {
                    99 => Intent::Escape,
                    _ => Intent::Attack,
                }
            }
            MonsterKind::GremlinFat => match self.next_move {
                99 => Intent::Escape,
                _ => Intent::AttackDebuff,
            },
            MonsterKind::GremlinTsundere => match self.next_move {
                1 => Intent::Defend,
                99 => Intent::Escape,
                _ => Intent::Attack,
            },
            MonsterKind::GremlinWizard => match self.next_move {
                1 => Intent::Attack, // 魔法爆发
                99 => Intent::Escape,
                _ => Intent::Unknown, // 充能
            },
            MonsterKind::GremlinLeader => match self.next_move {
                2 => Intent::Unknown,    // Rally 召唤
                3 => Intent::DefendBuff, // Encourage
                _ => Intent::Attack,     // Stab ×3
            },
            MonsterKind::BookOfStabbing => Intent::Attack,
            MonsterKind::Taskmaster => Intent::AttackDebuff,
            MonsterKind::BronzeOrb => match self.next_move {
                2 => Intent::Defend,
                3 => Intent::Debuff, // STASIS（STRONG_DEBUFF 归 Debuff）
                _ => Intent::Attack,
            },
            MonsterKind::BronzeAutomaton => match self.next_move {
                4 => Intent::Unknown, // 召唤铜球
                3 => Intent::Sleep,   // 放电后眩晕
                5 => Intent::DefendBuff,
                _ => Intent::Attack,
            },
            MonsterKind::TheCollector => match self.next_move {
                1 => Intent::Unknown, // 召唤火把头
                4 => Intent::Debuff,  // 超大减益
                5 => Intent::Unknown, // 复活
                3 => Intent::DefendBuff,
                _ => Intent::Attack, // 火球
            },
            MonsterKind::Champ => match self.next_move {
                2 => Intent::DefendBuff, // 防御姿态
                3 => Intent::Attack,     // 处决 ×2
                4 => Intent::AttackDebuff,
                5 | 7 => Intent::Buff, // 嘲弄 / 爆怒
                6 => Intent::Debuff,   // 奚落
                _ => Intent::Attack,   // 重斩
            },
            MonsterKind::Darkling => match self.next_move {
                2 => Intent::Defend,
                3 => Intent::Attack,  // NIP
                4 => Intent::Unknown, // 半死待复活
                5 => Intent::Buff,    // REINCARNATE
                _ => Intent::Attack,  // CHOMP ×2
            },
            MonsterKind::Repulsor => match self.next_move {
                1 => Intent::Debuff, // DAZE
                _ => Intent::Attack,
            },
            MonsterKind::OrbWalker => match self.next_move {
                1 => Intent::AttackDebuff, // LASER（+灼伤）
                _ => Intent::Attack,       // CLAW
            },
            MonsterKind::SpireGrowth => match self.next_move {
                2 => Intent::Debuff, // CONSTRICT（STRONG_DEBUFF 归 Debuff）
                _ => Intent::Attack,
            },
            MonsterKind::Transient => Intent::Attack,
            MonsterKind::Maw => match self.next_move {
                2 => Intent::Debuff, // ROAR
                4 => Intent::Buff,   // DROOL
                5 => Intent::Attack, // NOM ×N
                _ => Intent::Attack, // SLAM
            },
            MonsterKind::WrithingMass => match self.next_move {
                0 => Intent::Attack,       // BIG_HIT
                1 => Intent::Attack,       // MULTI ×3
                2 => Intent::AttackDefend, // ATTACK_BLOCK
                3 => Intent::AttackDebuff, // ATTACK_DEBUFF
                _ => Intent::Debuff,       // MEGA（寄生前置）
            },
            MonsterKind::GiantHead => match self.next_move {
                1 => Intent::Debuff, // GLARE
                2 => Intent::Attack, // IT_IS_TIME
                _ => Intent::Attack, // COUNT
            },
            MonsterKind::Nemesis => match self.next_move {
                3 => Intent::Attack, // SCYTHE 45
                4 => Intent::Debuff, // TRI_BURN
                _ => Intent::Attack, // TRI_ATTACK ×3
            },
            MonsterKind::Reptomancer => match self.next_move {
                1 => Intent::AttackDebuff, // SNAKE_STRIKE ×2 + 弱
                2 => Intent::Unknown,      // SPAWN
                _ => Intent::Attack,       // BIG_BITE
            },
            MonsterKind::SnakeDagger => Intent::Attack,
            MonsterKind::AwakenedOne => match self.next_move {
                3 => Intent::Unknown,      // 重生前待机
                5 => Intent::Attack,       // DARK_ECHO 40
                6 => Intent::AttackDebuff, // SLUDGE + 虚空
                _ => Intent::Attack,       // SLASH / SOUL_STRIKE / TACKLE
            },
            MonsterKind::Donu | MonsterKind::Deca => match self.next_move {
                0 => Intent::Attack, // BEAM ×2
                _ => Intent::Buff,   // CIRCLE
            },
            MonsterKind::TimeEater => match self.next_move {
                3 => Intent::DefendBuff, // RIPPLE（DEFEND_DEBUFF 归并）
                5 => Intent::Buff,       // 半血加速
                4 => Intent::AttackDebuff,
                _ => Intent::Attack,
            },
        }
    }

    /// 当前 next_move 的 DamageInfo 基值
    pub fn move_base_damage(&self) -> Option<i32> {
        match self.kind {
            MonsterKind::JawWorm => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Cultist => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::LouseNormal => match self.next_move {
                3 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::FungiBeast => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::LouseDefensive => match self.next_move {
                3 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::SpikeSlimeS => Some(self.dark_strike_dmg),
            MonsterKind::AcidSlimeS | MonsterKind::SpikeSlimeM => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::AcidSlimeM => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Looter | MonsterKind::SlaverBlue | MonsterKind::SlaverRed => {
                match self.next_move {
                    1 => Some(self.dark_strike_dmg),
                    4 => Some(self.thrash_dmg),
                    _ => None,
                }
            }
            MonsterKind::GremlinNob => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Lagavulin => match self.next_move {
                3 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::Sentry => match self.next_move {
                4 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::Guardian => match self.next_move {
                2 => Some(self.dark_strike_dmg),
                3 => Some(self.thrash_dmg),
                4 => Some(self.thrash_block),
                5 => Some(self.bellow_str),
                _ => None,
            },
            MonsterKind::SlimeBoss => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                _ => None,
            },
            MonsterKind::Hexaghost => match self.next_move {
                1 => Some(self.bellow_block),    // Divider：激活时锁定的段伤
                2 => Some(self.chomp_dmg),       // Tackle
                4 => Some(self.dark_strike_dmg), // Sear = 6
                6 => Some(self.thrash_dmg),      // Inferno 每段
                _ => None,
            },
            MonsterKind::SphericGuardian => match self.next_move {
                1 | 3 | 4 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::Chosen => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                5 => Some(self.thrash_block),
                _ => None,
            },
            MonsterKind::ShelledParasite => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                3 => Some(self.thrash_block),
                _ => None,
            },
            MonsterKind::Byrd => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                5 => Some(self.thrash_block),
                _ => None,
            },
            MonsterKind::Mugger => match self.next_move {
                1 => Some(self.chomp_dmg),
                4 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::SnakePlant => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::Snecko => match self.next_move {
                2 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Centurion => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Healer => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::GremlinWarrior
            | MonsterKind::GremlinThief
            | MonsterKind::GremlinFat
            | MonsterKind::GremlinTsundere
            | MonsterKind::GremlinWizard
            | MonsterKind::TorchHead => Some(self.chomp_dmg),
            MonsterKind::GremlinLeader => match self.next_move {
                4 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::BookOfStabbing => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Taskmaster => match self.next_move {
                2 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::BronzeOrb => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::BronzeAutomaton => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::TheCollector => match self.next_move {
                2 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::Champ => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                4 => Some(self.thrash_block),
                _ => None,
            },
            MonsterKind::Darkling => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Repulsor => match self.next_move {
                2 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::OrbWalker => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::SpireGrowth => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            // 转瞬即逝：起始死兆 + 已过回合数×10
            MonsterKind::Transient => Some(self.chomp_dmg + self.burn_upgraded * self.thrash_dmg),
            MonsterKind::Maw => match self.next_move {
                3 => Some(self.chomp_dmg),
                5 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::WrithingMass => match self.next_move {
                0 => Some(self.chomp_dmg),
                1 => Some(self.thrash_dmg),
                2 => Some(self.thrash_block),
                3 => Some(self.bellow_str),
                _ => None,
            },
            MonsterKind::GiantHead => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                2 => Some(self.thrash_dmg + (5 - self.burn_upgraded).max(0) * self.thrash_block),
                _ => None,
            },
            MonsterKind::Nemesis => match self.next_move {
                3 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::Reptomancer => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::SnakeDagger => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::AwakenedOne => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                5 => Some(self.thrash_block),
                6 => Some(self.bellow_str),
                8 => Some(self.bellow_block),
                _ => None,
            },
            MonsterKind::Donu | MonsterKind::Deca => match self.next_move {
                0 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::TimeEater => match self.next_move {
                2 => Some(self.chomp_dmg),
                4 => Some(self.thrash_dmg),
                _ => None,
            },
        }
    }
}
