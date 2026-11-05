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

/// 可玩角色（AbstractPlayer 子类；局外层参数化用）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CharKind {
    Ironclad,
    Silent,
    Defect,
    Watcher,
}

impl CharKind {
    /// 数据层 id（CHARACTERS / PotionHelper 键）
    pub fn id(&self) -> &'static str {
        match self {
            CharKind::Ironclad => "Ironclad",
            CharKind::Silent => "TheSilent",
            CharKind::Defect => "Defect",
            CharKind::Watcher => "Watcher",
        }
    }

    /// 卡池色（CardColor）
    pub fn color(&self) -> &'static str {
        match self {
            CharKind::Ironclad => "RED",
            CharKind::Silent => "GREEN",
            CharKind::Defect => "BLUE",
            CharKind::Watcher => "PURPLE",
        }
    }

    /// 基础最大生命（构造器）
    pub fn base_max_hp(&self) -> i32 {
        match self {
            CharKind::Ironclad => 80,
            CharKind::Silent => 70,
            CharKind::Defect => 75,
            CharKind::Watcher => 72,
        }
    }

    /// getAscensionMaxHPLoss（A14 减上限）：铁甲 5，其余 4
    pub fn asc_hp_loss(&self) -> i32 {
        match self {
            CharKind::Ironclad => 5,
            _ => 4,
        }
    }

    /// 初始遗物（starting_relics[0]）
    pub fn starting_relic(&self) -> crate::relics::RelicKind {
        use crate::relics::RelicKind;
        match self {
            CharKind::Ironclad => RelicKind::BurningBlood,
            CharKind::Silent => RelicKind::SnakeRing,
            CharKind::Defect => RelicKind::CrackedCore,
            CharKind::Watcher => RelicKind::PureWater,
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id.to_lowercase().as_str() {
            "ironclad" => Some(CharKind::Ironclad),
            "thesilent" | "silent" => Some(CharKind::Silent),
            "defect" => Some(CharKind::Defect),
            "watcher" => Some(CharKind::Watcher),
            _ => None,
        }
    }

    pub fn all() -> [CharKind; 4] {
        [
            CharKind::Ironclad,
            CharKind::Silent,
            CharKind::Defect,
            CharKind::Watcher,
        ]
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
    StrongDebuff,
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
    // —— Act 4（终局）——
    SpireShield,
    SpireSpear,
    CorruptHeart,
    // —— 补缺（Act 1 偏差清除）——
    AcidSlimeL,
    SpikeSlimeL,
    // —— 补缺（事件战斗 + Act3 形体组合）——
    /// 红面罩之孖：尖头小怪（固定 5×2 双段普攻）
    BanditPointy,
    /// 红面罩首领：嘲讽(空转) → 痛苦斩(伤+虚弱) → 交叉斩 循环（A17 交叉斩连打）
    BanditLeader,
    /// 红面罩熊：熊抱(敏捷-X) → 扑击(伤+自挡) → 撕咬 循环
    BanditBear,
    /// 荆棘形体：战前荆棘 3/4(A17+3)，普攻 7/9 或荆棘+2（5 次增益后恒普攻）
    Spiker,
    /// 爆炸形体：普攻 9/11 两回合后待机，第 3 回合自爆 30 伤（不计击杀）
    Exploder,
}

impl MonsterKind {
    /// 游戏内 monsterId（对应反编译类的 `public static final String ID`）。
    /// 用于与 bridge Obs 的 `id` 字段逐字节对拍。
    pub fn id(&self) -> &'static str {
        match self {
            MonsterKind::JawWorm => "JawWorm",
            MonsterKind::Cultist => "Cultist",
            MonsterKind::LouseNormal => "FuzzyLouseNormal",
            MonsterKind::LouseDefensive => "FuzzyLouseDefensive",
            MonsterKind::FungiBeast => "FungiBeast",
            MonsterKind::SpikeSlimeS => "SpikeSlime_S",
            MonsterKind::AcidSlimeS => "AcidSlime_S",
            MonsterKind::SpikeSlimeM => "SpikeSlime_M",
            MonsterKind::AcidSlimeM => "AcidSlime_M",
            MonsterKind::Looter => "Looter",
            MonsterKind::SlaverBlue => "SlaverBlue",
            MonsterKind::SlaverRed => "SlaverRed",
            MonsterKind::GremlinNob => "GremlinNob",
            MonsterKind::Lagavulin => "Lagavulin",
            MonsterKind::Sentry => "Sentry",
            // 真游戏 ID = TheGuardian（exordium/TheGuardian.java；曾误写 Guardian）
            MonsterKind::Guardian => "TheGuardian",
            MonsterKind::SlimeBoss => "SlimeBoss",
            MonsterKind::Hexaghost => "Hexaghost",
            MonsterKind::SphericGuardian => "SphericGuardian",
            MonsterKind::Chosen => "Chosen",
            MonsterKind::ShelledParasite => "Shelled Parasite",
            MonsterKind::Byrd => "Byrd",
            MonsterKind::Mugger => "Mugger",
            MonsterKind::SnakePlant => "SnakePlant",
            MonsterKind::Snecko => "Snecko",
            MonsterKind::Centurion => "Centurion",
            MonsterKind::Healer => "Healer",
            MonsterKind::GremlinWarrior => "GremlinWarrior",
            MonsterKind::GremlinThief => "GremlinThief",
            MonsterKind::GremlinFat => "GremlinFat",
            MonsterKind::GremlinTsundere => "GremlinTsundere",
            MonsterKind::GremlinWizard => "GremlinWizard",
            MonsterKind::GremlinLeader => "GremlinLeader",
            MonsterKind::BookOfStabbing => "BookOfStabbing",
            MonsterKind::Taskmaster => "SlaverBoss",
            MonsterKind::BronzeOrb => "BronzeOrb",
            MonsterKind::BronzeAutomaton => "BronzeAutomaton",
            MonsterKind::TorchHead => "TorchHead",
            MonsterKind::TheCollector => "TheCollector",
            MonsterKind::Champ => "Champ",
            MonsterKind::Darkling => "Darkling",
            MonsterKind::Repulsor => "Repulsor",
            MonsterKind::OrbWalker => "Orb Walker",
            MonsterKind::SpireGrowth => "Serpent",
            MonsterKind::Transient => "Transient",
            MonsterKind::Maw => "Maw",
            MonsterKind::WrithingMass => "WrithingMass",
            MonsterKind::GiantHead => "GiantHead",
            MonsterKind::Nemesis => "Nemesis",
            MonsterKind::Reptomancer => "Reptomancer",
            MonsterKind::SnakeDagger => "Dagger",
            MonsterKind::AwakenedOne => "AwakenedOne",
            MonsterKind::Donu => "Donu",
            MonsterKind::Deca => "Deca",
            MonsterKind::TimeEater => "TimeEater",
            MonsterKind::SpireShield => "SpireShield",
            MonsterKind::SpireSpear => "SpireSpear",
            MonsterKind::CorruptHeart => "CorruptHeart",
            MonsterKind::AcidSlimeL => "AcidSlime_L",
            MonsterKind::SpikeSlimeL => "SpikeSlime_L",
            MonsterKind::BanditPointy => "BanditChild",
            MonsterKind::BanditLeader => "BanditLeader",
            MonsterKind::BanditBear => "BanditBear",
            MonsterKind::Spiker => "Spiker",
            MonsterKind::Exploder => "Exploder",
        }
    }
}

/// 怪物构造参数（`MonsterState::spawn` 的产物之一）。
///
/// 为什么要有这个结构体：原先 spawn 返回裸 7 元组
/// `(chomp_dmg, thrash_dmg, thrash_block, bellow_str, bellow_block, dark_strike_dmg, ritual_amt)`，
/// 字段含义只能靠注释记忆 —— 已经因此踩过 4 次同一类 bug（Louse / SpikeSlime_M /
/// AcidSlime_S / Looter 都把伤害放进了 .0 而 take_turn 读 .5）。
/// 改成具名后编译器会直接拦住位置错误。
///
/// 字段复用约定（不同怪含义不同，见各自 spawn 分支的注释）：
/// - `chomp_dmg`      ：主攻击伤害（未使用 .5 的怪）
/// - `dark_strike_dmg`：主攻击伤害（大多数 exordium 怪）
/// - `thrash_dmg`     ：第二攻击伤害 / 金币额 等
/// - `thrash_block`   ：格挡值 / 第二攻击伤害 等
/// - `bellow_str` / `bellow_block`：增益数值
/// - `ritual_amt`     ：层数类数值（Ritual / Frail / Weak）
#[derive(Clone, Copy, Debug, Default)]
pub struct MonsterStats {
    pub chomp_dmg: i32,
    pub thrash_dmg: i32,
    pub thrash_block: i32,
    pub bellow_str: i32,
    pub bellow_block: i32,
    pub dark_strike_dmg: i32,
    pub ritual_amt: i32,
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
    /// Guardian：isOpen（true=Offensive 开放形态；false=Defensive 闭合形态+Sharp Hide）
    pub guardian_open: bool,
    /// Guardian：本相位已累计受击伤害（≥ 阈值触发模式切换）
    pub guardian_dmg_taken: i32,
    /// JawWorm hardMode（Jaw Worm Horde）：构造器置 firstMove=false（首招按 num 掷），
    /// 且 usePreBattleAction 叠 Strength(bellowStr) + Block(bellowBlock)
    pub hard_mode: bool,
}

impl MonsterState {
    /// Java `AbstractMonster.setHp(int hp)`（单参）= `setHp(hp, hp)` →
    /// `monsterHpRng.random(hp, hp)` = `nextInt(1)` ——**仍然消耗 1 次 monsterHpRng**。
    /// 干跑该次抽取后返回固定值（值恒为 hp，但流推进必须精确）。
    fn set_hp_fixed(hp_rng: &mut StsRandom, hp: i32) -> i32 {
        let _ = hp_rng.random_start_end(hp, hp);
        hp
    }

    /// 构造：按种类掷 HP（monsterHpRng 流，调用方传入保证流序）
    pub fn spawn(kind: MonsterKind, ascension: i32, hp_rng: &mut StsRandom) -> Self {
        let (hp, st) = match kind {
            MonsterKind::JawWorm => {
                let (chomp, td, tb, bs, bb, (lo, hi)) = Self::jaw_worm_stats(ascension);
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: chomp,
                        thrash_dmg: td,
                        thrash_block: tb,
                        bellow_str: bs,
                        bellow_block: bb,
                        dark_strike_dmg: 6,
                        ritual_amt: 3,
                    },
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
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 0,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 6,
                        ritual_amt: ritual,
                    },
                )
            }
            MonsterKind::LouseNormal => {
                // Java 顺序：先 setHp(min,max) 再 biteDamage（都在 monsterHpRng 上）
                let (lo, hi) = if ascension >= 7 { (11, 16) } else { (10, 15) };
                let hp = hp_rng.random_start_end(lo, hi);
                let bite = if ascension >= 2 {
                    hp_rng.random_start_end(6, 8)
                } else {
                    hp_rng.random_start_end(5, 7)
                };
                // ⚠️ MonsterState 的 7 元组字段序见 spawn 末尾的解构：
                //   .0 chomp_dmg  .1 thrash_dmg  .2 thrash_block  .3 bellow_str
                //   .4 bellow_block  .5 dark_strike_dmg  .6 ritual_amt
                // Louse 的 bite 走 monster_turn 的 `dark_strike_dmg` 分支，
                // 所以必须放 .5（原先放在 .0，导致咬击伤害恒为 0）。
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: 0,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: bite,
                        ritual_amt: 3,
                    },
                )
            }
            MonsterKind::FungiBeast => {
                // Java FungiBeast：BITE_DMG = 6（A2+ 仍 6）、GROW_STR = 3 / A2+ 4。
                // take_turn 的 bite=dark_strike_dmg、grow=ritual_amt，两者都要填
                //（曾错存进 chomp/thrash，导致 bite 打出 0——实测 floor11 玩家少掉 1 HP）。
                let (lo, hi) = if ascension >= 7 { (24, 28) } else { (22, 28) };
                let str_amt = if ascension >= 2 { 4 } else { 3 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 6,
                        thrash_dmg: str_amt,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 6,
                        ritual_amt: str_amt,
                    },
                )
            }
            MonsterKind::LouseDefensive => {
                let (lo, hi) = if ascension >= 7 { (12, 18) } else { (11, 17) };
                let hp = hp_rng.random_start_end(lo, hi);
                let bite = if ascension >= 2 {
                    hp_rng.random_start_end(6, 8)
                } else {
                    hp_rng.random_start_end(5, 7)
                };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: 0,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: bite,
                        ritual_amt: 2,
                    },
                )
            }
            MonsterKind::SpikeSlimeS => {
                let (lo, hi) = if ascension >= 7 { (11, 15) } else { (10, 14) };
                let dmg = if ascension >= 2 { 6 } else { 5 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: dmg,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            MonsterKind::AcidSlimeS => {
                // AcidSlime_S：setHp(8,12)/A7 (9,13)；TACKLE dmg A2+ 4 否则 3；
                // Weak 1 层（.6）。
                // ⚠️ 伤害放 .5（dark_strike_dmg）——move_base_damage / take_turn 读这里。
                let (lo, hi) = if ascension >= 7 { (9, 13) } else { (8, 12) };
                let dmg = if ascension >= 2 { 4 } else { 3 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 0,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: dmg,
                        ritual_amt: 1,
                    },
                )
            }
            MonsterKind::SpikeSlimeM => {
                // SpikeSlime_M：setHp(28,32)/A7 (29,34)；TACKLE dmg A2+ 10 否则 8。
                // ⚠️ 伤害必须放 .5（dark_strike_dmg）——take_turn / move_base_damage
                // 都读这个字段；放 .0 会让意图显示 0 伤害（同 Louse 踩过的坑）。
                let (lo, hi) = if ascension >= 7 { (29, 34) } else { (28, 32) };
                let dmg = if ascension >= 2 { 10 } else { 8 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 0,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: dmg,
                        ritual_amt: 1,
                    },
                )
            }
            MonsterKind::AcidSlimeM => {
                let (lo, hi) = if ascension >= 7 { (29, 34) } else { (28, 32) };
                let dmg = if ascension >= 2 { 8 } else { 7 };
                let dmg2 = if ascension >= 2 { 11 } else { 10 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: dmg,
                        thrash_dmg: 1,
                        thrash_block: dmg2,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            MonsterKind::Looter => {
                // Looter：setHp(44,48)/A7 (46,50)
                //   .0 chomp_dmg    = swipeDmg  10 / A2+ 11
                //   .1 thrash_dmg   = goldAmt   15 / A17+ 20（Thievery 层数）
                //   .2 thrash_block = lungeDmg  12 / A2+ 14
                let (lo, hi) = if ascension >= 7 { (46, 50) } else { (44, 48) };
                let swipe = if ascension >= 2 { 11 } else { 10 };
                let lunge = if ascension >= 2 { 14 } else { 12 };
                let gold = if ascension >= 17 { 20 } else { 15 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: swipe,
                        thrash_dmg: gold,
                        thrash_block: lunge,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            MonsterKind::SlaverBlue => {
                // Java SlaverBlue：Stab = 12（A2+ 13）、Skull Bash = 7（A2+ 8）+ Weak 1。
                // take_turn 的 stab=dark_strike_dmg、rake=thrash_dmg、weak=ritual_amt，
                // 曾错存进 chomp/thrash/thrash_block（floor12 实测玩家 0 伤害）。
                let (lo, hi) = if ascension >= 7 { (48, 52) } else { (46, 50) };
                let stab = if ascension >= 2 { 13 } else { 12 };
                let rake = if ascension >= 2 { 8 } else { 7 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: stab,
                        thrash_dmg: rake,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: stab,
                        ritual_amt: 1,
                    },
                )
            }
            MonsterKind::SlaverRed => {
                // Java SlaverRed：Stab = 13（A2+ 14）、Scrape = 8（A2+ 9）+ Vulnerable 2。
                // take_turn 的 stab=dark_strike_dmg、scrape=thrash_dmg、vuln=ritual_amt。
                let (lo, hi) = if ascension >= 7 { (48, 52) } else { (46, 50) };
                let stab = if ascension >= 2 { 14 } else { 13 };
                let scrape = if ascension >= 2 { 9 } else { 8 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: stab,
                        thrash_dmg: scrape,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: stab,
                        ritual_amt: 2,
                    },
                )
            }
            MonsterKind::GremlinNob => {
                // GremlinNob：setHp(82,86)/A8 (85,90)
                //   rushDmg = A3+ 16 否则 14   → move 1 (Bull Rush, ATTACK)
                //   bashDmg = A3+ 8  否则 6    → move 2 (Skull Bash, ATTACK_DEBUFF)
                //   Bellow(Anger) 层数 = A18+ 3 否则 2 → move 3 (BUFF)
                //   Skull Bash 的易伤层数固定 2
                // ⚠️ take_turn 读的是 dark_strike_dmg(move1) / thrash_dmg(move2)，
                //    原实现把 rush 放 chomp_dmg、bash 放 thrash_block → 两个都读不到。
                let (lo, hi) = if ascension >= 2 { (85, 90) } else { (82, 86) };
                let bash = if ascension >= 2 { 8 } else { 6 };
                let rush = if ascension >= 2 { 16 } else { 14 };
                let anger = if ascension >= 18 { 3 } else { 2 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        dark_strike_dmg: rush, // move 1
                        thrash_dmg: bash,      // move 2
                        bellow_str: 2,         // 易伤层数
                        ritual_amt: anger,     // Bellow 的 Anger 层数
                        ..Default::default()
                    },
                )
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
                    MonsterStats {
                        chomp_dmg: dmg,
                        thrash_dmg: debuff,
                        thrash_block: 0,
                        bellow_str: 8,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            MonsterKind::Sentry => {
                // Java Sentry：Beam 攻击 = 9（A3+ 10）；Debuff 版加 2 张 Dazed。
                // take_turn 的 beam=dark_strike_dmg、dazed 数=ritual_amt。
                let (lo, hi) = if ascension >= 2 { (39, 45) } else { (38, 42) };
                let beam = if ascension >= 3 { 10 } else { 9 };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: beam,
                        thrash_dmg: 2,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: beam,
                        ritual_amt: 2,
                    },
                )
            }
            MonsterKind::Guardian => {
                // Java TheGuardian：setHp(240)（1 参 setHp 仍消耗 monsterHpRng.random(240,240)
                // = nextInt(1) 1 次）。曾不掷，实测 PMCWKZEF floor16 monsterHp 0 vs 真 1。
                let hp = {
                    let _rolled = hp_rng.random_start_end(240, 240);
                    if ascension >= 2 {
                        250
                    } else {
                        240
                    }
                };
                let bash = if ascension >= 2 { 36 } else { 32 };
                let roll = if ascension >= 2 { 10 } else { 9 };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: bash,
                        thrash_dmg: roll,
                        thrash_block: 5,
                        bellow_str: 8,
                        bellow_block: 9,
                        dark_strike_dmg: 2,
                        ritual_amt: 4,
                    },
                )
            }
            MonsterKind::SlimeBoss => {
                // SlimeBoss：HP 提升在 **A9**（150/140，曾误用 A2）；
                // Slam 提升与 tackle 在 **A4**（38/35、10/9，曾误用 A2）；
                // Goop 的粘液张数 A19+ = 5，否则 3（曾误写成 10）。
                // 槽位：chomp=slam（move 1 用）、thrash_dmg=goop 张数
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 150 } else { 140 });
                let slam = if ascension >= 4 { 38 } else { 35 };
                let goop = if ascension >= 19 { 5 } else { 3 };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: slam,
                        thrash_dmg: goop,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 六火亡魂：字段映射 —— chomp=火焰冲撞/撞，thrash=地狱火(每段)，
            // thrash_block=煽燃力量，bellow_str=灼烙灼伤数，bellow_block=分割者段伤
            //（激活回合锁定=玩家 HP/12+1），dark_strike=灼烙伤 6，ritual=已激活球数
            MonsterKind::Hexaghost => {
                // Boss 血量提升在 A9（构造器：>=9 → 264，否则 250）
                // this.setHp(单参) → 仍耗 1 次 monsterHpRng
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 264 } else { 250 });
                if ascension >= 19 {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 6,
                            thrash_dmg: 3,
                            thrash_block: 3,
                            bellow_str: 2,
                            bellow_block: 0,
                            dark_strike_dmg: 6,
                            ritual_amt: 0,
                        },
                    )
                } else if ascension >= 4 {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 6,
                            thrash_dmg: 3,
                            thrash_block: 2,
                            bellow_str: 1,
                            bellow_block: 0,
                            dark_strike_dmg: 6,
                            ritual_amt: 0,
                        },
                    )
                } else {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 5,
                            thrash_dmg: 2,
                            thrash_block: 2,
                            bellow_str: 1,
                            bellow_block: 0,
                            dark_strike_dmg: 6,
                            ritual_amt: 0,
                        },
                    )
                }
            }
            // 球形守护者：HP 固定 20；开局壁垒+神器3+40格挡（战斗外初始化见 new_with）
            // 字段：chomp=伤害(10/11) thrash=充能格挡(25/35) thrash_block=硬化(15)
            // bellow_str=脆弱(5)
            MonsterKind::SphericGuardian => (
                20,
                MonsterStats {
                    chomp_dmg: if ascension >= 2 { 11 } else { 10 },
                    thrash_dmg: if ascension >= 17 { 35 } else { 25 },
                    thrash_block: 15,
                    bellow_str: 5,
                    ..Default::default()
                },
            ),
            // 天选者：chomp=电击(18/21) thrash=衰弱(10/12) thrash_block=戳刺(5/6)
            // bellow_str=汲取力量(3) bellow_block=汲取虚弱(3) dark=易伤(2) ritual=咒缚(1)
            MonsterKind::Chosen => {
                let (lo, hi) = if ascension >= 7 { (98, 103) } else { (95, 99) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 21 } else { 18 },
                        thrash_dmg: if ascension >= 2 { 12 } else { 10 },
                        thrash_block: if ascension >= 2 { 6 } else { 5 },
                        bellow_str: 3,
                        bellow_block: 3,
                        dark_strike_dmg: 2,
                        ritual_amt: 1,
                    },
                )
            }
            // 灼壳寄生虫：开局多层护甲14+14格挡；chomp=猛击(18/21)
            // thrash=双重打击(6/7) thrash_block=吸取(10/12) bellow_str=脆弱(2)
            MonsterKind::ShelledParasite => {
                let (lo, hi) = if ascension >= 7 { (70, 75) } else { (68, 72) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 21 } else { 18 },
                        thrash_dmg: if ascension >= 2 { 7 } else { 6 },
                        thrash_block: if ascension >= 2 { 12 } else { 10 },
                        bellow_str: 2,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 伯德鸟：chomp=啄(1) thrash=俯冲(12/14) thrash_block=头槌(3)
            // bellow_str=飞行层数(3/4) bellow_block=啄次数(5/6) ritual=鸣叫力量(1)
            MonsterKind::Byrd => {
                let (lo, hi) = if ascension >= 7 { (26, 33) } else { (25, 31) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 1,
                        thrash_dmg: if ascension >= 2 { 14 } else { 12 },
                        thrash_block: 3,
                        bellow_str: if ascension >= 17 { 4 } else { 3 },
                        bellow_block: if ascension >= 2 { 6 } else { 5 },
                        dark_strike_dmg: 0,
                        ritual_amt: 1,
                    },
                )
            }
            // 劫匪：chomp=抢劫(10/11) thrash=重扫(16/18) thrash_block=烟雾(11)
            // bellow_str=偷金(15/20)；A17 烟雾格挡+6
            MonsterKind::Mugger => {
                let (lo, hi) = if ascension >= 7 { (50, 54) } else { (48, 52) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 11 } else { 10 },
                        thrash_dmg: if ascension >= 2 { 18 } else { 16 },
                        thrash_block: 11,
                        bellow_str: if ascension >= 17 { 20 } else { 15 },
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 咬人蛇草：chomp=撕咬(7/8)×3 bellow_str=孢子脆弱(2) bellow_block=孢子虚弱(2)
            MonsterKind::SnakePlant => {
                let (lo, hi) = if ascension >= 7 { (78, 82) } else { (75, 79) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 8 } else { 7 },
                        thrash_dmg: 3,
                        thrash_block: 0,
                        bellow_str: 2,
                        bellow_block: 2,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
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
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 18 } else { 15 },
                        thrash_dmg: if ascension >= 2 { 10 } else { 8 },
                        thrash_block: 0,
                        bellow_str: 2,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 百夫长：chomp=斩击(12/14) thrash=狂怒(6/7)×3 bellow_str=段数(3)
            // bellow_block=护卫格挡(15/20)
            MonsterKind::Centurion => {
                let (lo, hi) = if ascension >= 7 { (78, 83) } else { (76, 80) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 14 } else { 12 },
                        thrash_dmg: if ascension >= 2 { 7 } else { 6 },
                        thrash_block: 3,
                        bellow_str: 0,
                        bellow_block: if ascension >= 17 { 20 } else { 15 },
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 拜蛇教祭司：chomp=魔法(8/9) thrash=治疗(16) bellow_str=力量(2/3)
            // bellow_block=脆弱(2)
            MonsterKind::Healer => {
                let (lo, hi) = if ascension >= 7 { (50, 58) } else { (48, 56) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 9 } else { 8 },
                        thrash_dmg: 16,
                        thrash_block: if ascension >= 17 { 3 } else { 2 },
                        bellow_str: 2,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // ════════ Act 2 精英/Boss 及召唤物 ════════
            // 战鬼：chomp=抓击(4/5)；战前 Angry 1(2 A17)
            MonsterKind::GremlinWarrior => {
                let (lo, hi) = if ascension >= 7 { (21, 25) } else { (20, 24) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 5 } else { 4 },
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 暴徒：chomp=穿刺(9/10)
            MonsterKind::GremlinThief => {
                let (lo, hi) = if ascension >= 7 { (11, 15) } else { (10, 14) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 10 } else { 9 },
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 胖鬼：chomp=钝击(4/5)+弱1(A17 再+脆1)：thrash=弱1 thrash_block=脆1
            MonsterKind::GremlinFat => {
                let (lo, hi) = if ascension >= 7 { (14, 18) } else { (13, 17) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 5 } else { 4 },
                        thrash_dmg: 1,
                        thrash_block: 1,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
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
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 8 } else { 6 },
                        thrash_dmg: block,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 巫鬼：chomp=魔法(25/30)；counter=currentCharge 起 1
            MonsterKind::GremlinWizard => {
                let (lo, hi) = if ascension >= 7 { (22, 26) } else { (21, 25) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 30 } else { 25 },
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
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
                    MonsterStats {
                        chomp_dmg: 6,
                        thrash_dmg: str_amt,
                        thrash_block: block_amt,
                        bellow_str: 3,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
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
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: stab,
                        thrash_dmg: big,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 监工：chomp=鞭笞7 + thrash=伤口数(1/2 A3/3 A18)（A18 再自+1 力量）
            MonsterKind::Taskmaster => {
                // 真游戏 super(..., monsterHpRng.random(54, 60), ...)
                // + 随后 setHp(...) —— **2 次消耗**（首次 HP 被覆盖，但流必须推进）
                let _super_hp = hp_rng.random_start_end(54, 60);
                let (lo, hi) = if ascension >= 8 { (57, 64) } else { (54, 60) };
                let wounds = if ascension >= 18 {
                    3
                } else if ascension >= 3 {
                    2
                } else {
                    1
                };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 7,
                        thrash_dmg: wounds,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 铜球：chomp=光束8 thrash=支援格挡12；STASIS 吸牌（死亡归还）
            MonsterKind::BronzeOrb => {
                // 真游戏 super(..., monsterHpRng.random(52, 58), ...)
                // + 随后 setHp(...) —— **2 次消耗**（首次 HP 被覆盖，但流必须推进）
                let _super_hp = hp_rng.random_start_end(52, 58);
                let (lo, hi) = if ascension >= 9 { (54, 60) } else { (52, 58) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 8,
                        thrash_dmg: 12,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 铜制自动机：chomp=连枷(7/8)×2 thrash=超究光束(45/50) thrash_block=强化格挡(9/12)
            // bellow_str=强化力量(3/4)；counter=回合计；战前神器3
            MonsterKind::BronzeAutomaton => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 320 } else { 300 });
                let (flail, beam, str_amt) = if ascension >= 4 {
                    (8, 50, 4)
                } else {
                    (7, 45, 3)
                };
                let block_amt = if ascension >= 9 { 12 } else { 9 };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: flail,
                        thrash_dmg: beam,
                        thrash_block: block_amt,
                        bellow_str: str_amt,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 火把头：chomp=冲撞7（恒定，SetMove 驱动）
            MonsterKind::TorchHead => {
                // 真游戏 super(..., monsterHpRng.random(60, 70), ...)
                // + 随后 setHp(...) —— **2 次消耗**（首次 HP 被覆盖，但流必须推进）
                let _super_hp = hp_rng.random_start_end(60, 70);
                let (lo, hi) = if ascension >= 9 { (40, 45) } else { (38, 40) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 7,
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 收集者：chomp=火球(18/21) thrash=群力(3/4/5) thrash_block=自我格挡(15/18)
            // bellow_str=超大减益(3/5 A19)；turns=history.len()
            MonsterKind::TheCollector => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 300 } else { 282 });
                let (fb, str_amt, mega) = if ascension >= 19 {
                    (21, 5, 5)
                } else if ascension >= 4 {
                    (21, 4, 3)
                } else {
                    (18, 3, 3)
                };
                let block_amt = if ascension >= 9 { 18 } else { 15 };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: fb,
                        thrash_dmg: str_amt,
                        thrash_block: block_amt,
                        bellow_str: mega,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 冠军：chomp=重斩(16/18) thrash=处决10×2 thrash_block=扇击(12/14)
            // bellow_str=力量(2/3/4) bellow_block=锻造金属化(5/6/7) dark_strike=格挡(15/18/20)
            MonsterKind::Champ => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 440 } else { 420 });
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
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: slash,
                        thrash_dmg: 10,
                        thrash_block: slap,
                        bellow_str: str_amt,
                        bellow_block: forge,
                        dark_strike_dmg: block_amt,
                        ritual_amt: 0,
                    },
                )
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
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 9 } else { 8 },
                        thrash_dmg: nip,
                        thrash_block: 12,
                        bellow_str: 2,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 排斥体：chomp=攻击(11/13) thrash=晕眩卡数2
            MonsterKind::Repulsor => {
                let (lo, hi) = if ascension >= 7 { (31, 38) } else { (29, 35) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 13 } else { 11 },
                        thrash_dmg: 2,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 天球行者：chomp=激光(10/11) thrash=利爪(15/16)；战前力量 3(5 A17)
            MonsterKind::OrbWalker => {
                // 真游戏 super(..., monsterHpRng.random(90, 96), ...)
                // + 随后 setHp(...) —— **2 次消耗**（首次 HP 被覆盖，但流必须推进）
                let _super_hp = hp_rng.random_start_end(90, 96);
                let (lo, hi) = if ascension >= 7 { (92, 102) } else { (90, 96) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 11 } else { 10 },
                        thrash_dmg: if ascension >= 2 { 16 } else { 15 },
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 尖塔生长体：chomp=快击(16/18) thrash=重击(22/25) thrash_block=紧缠(10)
            MonsterKind::SpireGrowth => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 7 { 190 } else { 170 });
                let (tackle, smash) = if ascension >= 2 { (18, 25) } else { (16, 22) };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: tackle,
                        thrash_dmg: smash,
                        thrash_block: 10,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 转瞬即逝：chomp=起始死兆(30/40) thrash=每回合增量10；counter=count；
            // 战前 Fading 5(6 A17) + Shifting
            MonsterKind::Transient => (
                999,
                MonsterStats {
                    chomp_dmg: if ascension >= 2 { 40 } else { 30 },
                    thrash_dmg: 10,
                    ..Default::default()
                },
            ),
            // 巨口：chomp=猛击(25/30) thrash=啃食5 thrash_block=咆哮时长3；
            // bellow_str=流涎力量3；counter=turnCount 起 1
            // 巨口：super 常量 300（无 setHp → **0 次 monsterHpRng**）
            MonsterKind::Maw => (
                300,
                MonsterStats {
                    chomp_dmg: if ascension >= 2 { 30 } else { 25 },
                    thrash_dmg: 5,
                    thrash_block: 3,
                    bellow_str: 3,
                    ..Default::default()
                },
            ),
            // 蠕动聚块：chomp=巨击(32/38) thrash=连击(7/9)×3 thrash_block=攻防(15/16)
            // bellow_str=削弱攻击(10/12) bellow_block=弱易2；战前 Reactive+Malleable
            MonsterKind::WrithingMass => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 7 { 175 } else { 160 });
                if ascension >= 2 {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 38,
                            thrash_dmg: 9,
                            thrash_block: 16,
                            bellow_str: 12,
                            bellow_block: 2,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                } else {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 32,
                            thrash_dmg: 7,
                            thrash_block: 15,
                            bellow_str: 10,
                            bellow_block: 2,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                }
            }
            // ════════ Act 3 精英/Boss 及召唤物 ══════════
            // 巨首：chomp=计数打击13 thrash=死兆(30/40 A3) thrash_block=每层+5；
            // counter=count 起 5；战前自身 Slow
            MonsterKind::GiantHead => {
                let hp = if ascension >= 8 { 520 } else { 500 };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: 13,
                        thrash_dmg: if ascension >= 3 { 40 } else { 30 },
                        thrash_block: 5,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 复仇者：chomp=镰刀45 thrash=三连火(6/7 A3)×3 thrash_block=灼伤(3/5 A18)；
            // counter=镰刀冷却；每回合尾自动无形 1
            MonsterKind::Nemesis => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 8 { 200 } else { 185 });
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: 45,
                        thrash_dmg: if ascension >= 3 { 7 } else { 6 },
                        thrash_block: if ascension >= 18 { 5 } else { 3 },
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
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
                        MonsterStats {
                            chomp_dmg: 16,
                            thrash_dmg: 34,
                            thrash_block: if ascension >= 18 { 2 } else { 1 },
                            bellow_str: 1,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                } else {
                    (
                        hp_rng.random_start_end(lo, hi),
                        MonsterStats {
                            chomp_dmg: 13,
                            thrash_dmg: 30,
                            thrash_block: if ascension >= 18 { 2 } else { 1 },
                            bellow_str: 1,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                }
            }
            // 蛇匕：chomp=刺9 thrash=自爆25（打完自灭）
            MonsterKind::SnakeDagger => {
                let (lo, hi) = (20, 25);
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: 9,
                        thrash_dmg: 25,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 觉醒者：双阶段(300/300, A9 320)；chomp=挥砍20 thrash=灵魂冲击6×4
            // thrash_block=黑暗回响40 bellow_str=污泥18 bellow_block=冲撞10
            // dark=冲击段数4 ritual=冲撞段数3；counter=阶段2标记
            MonsterKind::AwakenedOne => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 320 } else { 300 });
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: 20,
                        thrash_dmg: 6,
                        thrash_block: 40,
                        bellow_str: 18,
                        bellow_block: 10,
                        dark_strike_dmg: 4,
                        ritual_amt: 3,
                    },
                )
            }
            // 多努：chomp=光束(10/12)×2 thrash=力量环3；起手力量环；神器2(3 A19)
            MonsterKind::Donu => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 265 } else { 250 });
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: if ascension >= 4 { 12 } else { 10 },
                        thrash_dmg: 3,
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 德卡：chomp=光束(10/12)×2+晕2 thrash=护甲环3；起手光束；神器2(3 A19)
            MonsterKind::Deca => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 265 } else { 250 });
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: if ascension >= 4 { 12 } else { 10 },
                        thrash_dmg: 3,
                        thrash_block: 2,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 吞时者：chomp=回响(7/8)×3 thrash=头撞(26/32)+下回合抽牌-1
            // thrash_block=涟漪格挡20；半血一次加速；战前 TimeWarp
            MonsterKind::TimeEater => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 480 } else { 456 });
                let (reverb, slam) = if ascension >= 2 { (8, 32) } else { (7, 26) };
                (
                    hp,
                    MonsterStats {
                        chomp_dmg: reverb,
                        thrash_dmg: slam,
                        thrash_block: 20,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // ══════════ Act 4（终局）══════════
            // 尖塔之盾：chomp=痛击(12/14 A3+) thrash=粉碎(34/38+自甲)
            // thrash_block=筑防 30；moveCount 由史长推导；A18 神器 2
            MonsterKind::SpireShield => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 8 { 125 } else { 110 });
                if ascension >= 3 {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 14,
                            thrash_dmg: 38,
                            thrash_block: 30,
                            bellow_str: 0,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                } else {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 12,
                            thrash_dmg: 34,
                            thrash_block: 30,
                            bellow_str: 0,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                }
            }
            // 尖塔之矛：chomp=燃烧打击(5/6)×2+灼伤2 thrash=穿刺10
            // thrash_block=灼伤数2 bellow_str=穿刺段数(3/4)；A18 神器 2
            MonsterKind::SpireSpear => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 8 { 180 } else { 160 });
                if ascension >= 3 {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 6,
                            thrash_dmg: 10,
                            thrash_block: 2,
                            bellow_str: 4,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                } else {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 5,
                            thrash_dmg: 10,
                            thrash_block: 2,
                            bellow_str: 3,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                }
            }
            // 腐朽之心：chomp=回响(40/45 A4+) thrash=血射每段2
            // thrash_block=血射段数(12/15) bellow_str=削弱2；
            // moveCount/buffCount 由招式史推导；战前无敌300(200)+亡律1(2)
            MonsterKind::CorruptHeart => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 9 { 800 } else { 750 });
                if ascension >= 4 {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 45,
                            thrash_dmg: 2,
                            thrash_block: 15,
                            bellow_str: 2,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                } else {
                    (
                        hp,
                        MonsterStats {
                            chomp_dmg: 40,
                            thrash_dmg: 2,
                            thrash_block: 12,
                            bellow_str: 2,
                            bellow_block: 0,
                            dark_strike_dmg: 0,
                            ritual_amt: 0,
                        },
                    )
                }
            }
            // 大酸液史莱姆：chomp=腐蚀冲撞(11/12)+粘液2 thrash=普通冲撞(16/18)
            // bellow_block=弱舔2；半血分裂为 2 只 M 版（各承当前 HP）
            MonsterKind::AcidSlimeL => {
                let (lo, hi) = if ascension >= 7 { (68, 72) } else { (65, 69) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 12 } else { 11 },
                        thrash_dmg: if ascension >= 2 { 18 } else { 16 },
                        thrash_block: 2,
                        bellow_str: 2,
                        bellow_block: 2,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // 大尖刺史莱姆：chomp=火焰冲撞(16/18)+粘液2 bellow_str=脆舔(2/3 A17)
            // 半血分裂为 2 只 M 版
            MonsterKind::SpikeSlimeL => {
                let (lo, hi) = if ascension >= 7 { (67, 73) } else { (64, 70) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 18 } else { 16 },
                        thrash_dmg: 0,
                        thrash_block: if ascension >= 17 { 3 } else { 2 },
                        bellow_str: 2,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: 0,
                    },
                )
            }
            // ── 红面罩三人组（BanditPointy/Leader/Bear.java 逐行）──
            // 字段映射：chomp=主伤，dark_strike=次伤/段数，thrash=三号伤，
            // thrash_block=自挡，bellow_str=敏捷减益/荆棘计数，bellow_block=荆棘基数,
            // ritual_amt=虚弱量/段数
            MonsterKind::BanditPointy => {
                // 真游戏 this.setHp(单参) → 仍耗 1 次 monsterHpRng（nextInt(1)）
                let hp = Self::set_hp_fixed(hp_rng, if ascension >= 7 { 34 } else { 30 });
                (
                    hp,
                    MonsterStats {
                        // chomp=普攻伤，dark_strike=双段（段数放在 ritual_amt）
                        chomp_dmg: if ascension >= 2 { 6 } else { 5 },
                        dark_strike_dmg: 2,
                        ..Default::default()
                    },
                )
            }
            MonsterKind::BanditLeader => {
                let (lo, hi) = if ascension >= 7 { (37, 41) } else { (35, 39) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 17 } else { 15 }, // chomp=交叉斩
                        thrash_dmg: if ascension >= 2 { 12 } else { 10 }, // thrash=痛苦斩
                        thrash_block: 0,
                        bellow_str: 0,
                        bellow_block: 0,
                        dark_strike_dmg: 0,
                        ritual_amt: if ascension >= 17 { 3 } else { 2 }, // ritual=虚弱量
                    },
                )
            }
            MonsterKind::BanditBear => {
                let (lo, hi) = if ascension >= 7 { (40, 44) } else { (38, 42) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                    chomp_dmg: if ascension >= 2 { 20 } else { 18 },
                    thrash_dmg: // chomp=撕咬
                        0,
                    thrash_block: 9,
                    bellow_str: // thrash_block=扑击自挡
                        if ascension >= 17 { -4 } else { -2 },
                    bellow_block: // bellow_str=熊抱敏捷减益
                        0,
                    dark_strike_dmg: if ascension >= 2 { 10 } else { 9 },
                    ritual_amt: // dark_strike=扑击
                        0,
                },
                )
            }
            // ── 古形体（Spiker/Exploder.java 逐行）──
            MonsterKind::Spiker => {
                let (lo, hi) = if ascension >= 7 { (44, 60) } else { (42, 56) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                        chomp_dmg: if ascension >= 2 { 9 } else { 7 }, // chomp=普攻伤
                        thrash_dmg: 0,
                        thrash_block: 0,
                        bellow_str: 0, // bellow_str=已用增益数（prebattle 归零）
                        bellow_block: if ascension >= 2 { 4 } else { 3 }, // bellow_block=战前荆棘基数
                        dark_strike_dmg: 0,
                        ritual_amt: 2, // ritual=荆棘增益量
                    },
                )
            }
            MonsterKind::Exploder => {
                let (lo, hi) = if ascension >= 7 { (30, 35) } else { (30, 30) };
                (
                    hp_rng.random_start_end(lo, hi),
                    MonsterStats {
                    chomp_dmg: if ascension >= 2 { 11 } else { 9 },
                    thrash_dmg: // chomp=普攻伤
                        0,
                    thrash_block: 0,
                    bellow_str: 0,
                    bellow_block: 30,
                    dark_strike_dmg: // bellow_block=自爆伤（ExplosivePower）
                        0,
                    ritual_amt: 0,
                },
                )
            }
        };
        Self {
            kind,
            creature: Creature::new(hp, hp),
            next_move: 1,
            move_history: Vec::new(),
            first_move: true,
            chomp_dmg: st.chomp_dmg,
            thrash_dmg: st.thrash_dmg,
            thrash_block: st.thrash_block,
            bellow_str: st.bellow_str,
            bellow_block: st.bellow_block,
            dark_strike_dmg: st.dark_strike_dmg,
            ritual_amt: st.ritual_amt,
            // 通用计数器：Hexaghost=灼伤升级标志 / Mugger=已偷金币；其余怪构 0
            burn_upgraded: 0,
            escaped: false,
            stasis_card: None,
            half_dead: false,
            guardian_open: kind == MonsterKind::Guardian,
            guardian_dmg_taken: 0,
            hard_mode: false,
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
                1 => Intent::AttackDebuff, // Tackle + 1 张 Slimed 入弃牌堆
                _ => Intent::Debuff,       // Frail 1
            },
            MonsterKind::AcidSlimeM => match self.next_move {
                1 => Intent::AttackDebuff, // WOUND：攻击 + 塞 1 张粘液（ATTACK_DEBUFF）
                2 => Intent::Attack,       // TACKLE
                _ => Intent::Debuff,       // WEAK
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
                1 => Intent::Buff,         // Close Up（Sharp Hide）
                2 => Intent::Attack,       // Fierce Bash 32
                3 => Intent::Attack,       // Roll Attack 9
                4 => Intent::AttackBuff,   // Twin Slam 8×2
                5 => Intent::Attack,       // Whirlwind 5×4
                6 => Intent::Defend,       // Charge Up +9 格挡
                7 => Intent::StrongDebuff, // Vent Steam：Weak 2 + Vulnerable 2
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
            MonsterKind::SpireShield => match self.next_move {
                1 => Intent::AttackDebuff, // BASH（-1 力量）
                2 => Intent::Defend,       // FORTIFY
                _ => Intent::AttackDefend, // SMASH
            },
            MonsterKind::SpireSpear => match self.next_move {
                1 => Intent::AttackDebuff, // BURN_STRIKE
                2 => Intent::Buff,         // PIERCER
                _ => Intent::Attack,       // SKEWER ×N
            },
            MonsterKind::CorruptHeart => match self.next_move {
                3 => Intent::Debuff, // DEBILITATE（开场）
                4 => Intent::Buff,   // 增益轮转
                _ => Intent::Attack, // ECHO / BLOOD_SHOTS
            },
            MonsterKind::AcidSlimeL => match self.next_move {
                1 => Intent::AttackDebuff, // 腐蚀冲撞+粘液
                2 => Intent::Attack,       // 普通冲撞
                4 => Intent::Debuff,       // 弱舔
                _ => Intent::Unknown,      // 分裂
            },
            MonsterKind::SpikeSlimeL => match self.next_move {
                1 => Intent::AttackDebuff, // 火焰冲撞+粘液
                4 => Intent::Debuff,       // 脆舔
                _ => Intent::Unknown,      // 分裂
            },
            MonsterKind::BanditPointy => Intent::Attack, // 恒 5×2 双段
            MonsterKind::BanditLeader => match self.next_move {
                1 => Intent::Attack,       // 交叉斩
                3 => Intent::AttackDebuff, // 痛苦斩（伤+虚弱）
                _ => Intent::Unknown,      // 嘲讽空转
            },
            MonsterKind::BanditBear => match self.next_move {
                1 => Intent::Attack,       // 撕咬
                2 => Intent::Debuff,       // 熊抱（STRONG_DEBUFF）
                _ => Intent::AttackDefend, // 扑击（伤+自挡）
            },
            MonsterKind::Spiker => match self.next_move {
                1 => Intent::Attack,
                _ => Intent::Buff, // 荆棘+2
            },
            MonsterKind::Exploder => match self.next_move {
                1 => Intent::Attack,
                _ => Intent::Unknown, // 待机（第三回合自爆）
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
                1 => Some(self.chomp_dmg),    // WOUND：damage[0]（7/8）
                2 => Some(self.thrash_block), // TACKLE：damage[1]（10/11）
                _ => None,
            },
            MonsterKind::Looter => match self.next_move {
                1 => Some(self.chomp_dmg),    // swipeDmg
                4 => Some(self.thrash_block), // lungeDmg
                _ => None,
            },
            MonsterKind::SlaverBlue | MonsterKind::SlaverRed => match self.next_move {
                1 => Some(self.dark_strike_dmg),
                4 => Some(self.thrash_dmg),
                _ => None,
            },
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
                // 槽位与 spawn 对齐：fierce=chomp、roll=thrash、twin=bellow_str、
                // whirl=thrash_block（曾用 dark_strike=VENT 数=0，意图伤害显示 0）
                2 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                4 => Some(self.bellow_str),
                5 => Some(self.thrash_block),
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
            MonsterKind::SpireShield => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::SpireSpear => match self.next_move {
                1 => Some(self.chomp_dmg),
                3 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::CorruptHeart => match self.next_move {
                2 => Some(self.chomp_dmg),
                1 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::AcidSlimeL => match self.next_move {
                1 => Some(self.chomp_dmg),
                2 => Some(self.thrash_dmg),
                _ => None,
            },
            MonsterKind::SpikeSlimeL => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::BanditPointy => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::BanditLeader => match self.next_move {
                1 => Some(self.chomp_dmg),  // 交叉斩
                3 => Some(self.thrash_dmg), // 痛苦斩
                _ => None,
            },
            MonsterKind::BanditBear => match self.next_move {
                1 => Some(self.chomp_dmg),       // 撕咬
                3 => Some(self.dark_strike_dmg), // 扑击
                _ => None,
            },
            MonsterKind::Spiker => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
            MonsterKind::Exploder => match self.next_move {
                1 => Some(self.chomp_dmg),
                _ => None,
            },
        }
    }
}
