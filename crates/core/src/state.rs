//! 战斗状态结构。

use crate::cards::CardInst;
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Attack,
    DefendBuff,
    AttackDefend,
    Buff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonsterKind {
    JawWorm,
    Cultist,
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
        }
    }
}
