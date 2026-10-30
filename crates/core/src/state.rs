//! 战斗状态结构。

use crate::cards::CardInst;
use crate::powers::Power;

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

    /// decrementBlock + 扣血的 M2 简化（M2 无 onAttacked 改伤类 power）
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

#[derive(Clone, Debug)]
pub struct PlayerState {
    pub creature: Creature,
    pub energy: i32,
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
}

/// 颚虫（monsters/exordium/JawWorm.java 的数值字段）
#[derive(Clone, Debug)]
pub struct JawWormState {
    pub creature: Creature,
    pub next_move: u8,
    pub move_history: Vec<u8>,
    pub first_move: bool,
    pub chomp_dmg: i32,
    pub thrash_dmg: i32,
    pub thrash_block: i32,
    pub bellow_str: i32,
    pub bellow_block: i32,
}

impl JawWormState {
    /// 按进阶分档：
    /// (chomp, thrashDmg, thrashBlock, bellowStr, bellowBlock, hpRange)
    pub fn stats(ascension: i32) -> (i32, i32, i32, i32, i32, (i32, i32)) {
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
        match self.next_move {
            1 => Intent::Attack,
            2 => Intent::DefendBuff,
            3 => Intent::AttackDefend,
            _ => Intent::Attack,
        }
    }

    /// 当前 next_move 的 DamageInfo 基值
    pub fn move_base_damage(&self) -> Option<i32> {
        match self.next_move {
            1 => Some(self.chomp_dmg),
            3 => Some(self.thrash_dmg),
            _ => None,
        }
    }
}
