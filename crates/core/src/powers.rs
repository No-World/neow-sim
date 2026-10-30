//! power（增益/减益）—— 逐位复刻反编译源码（powers/VulnerablePower.java 等）。
//!
//! 语义来源（docs/engine-semantics.md §7 + 源码直读）：
//! - 易伤 Vulnerable：受击方 `atDamageReceive ×1.5`（仅 NORMAL）
//! - 虚弱 Weakened：造成方 `atDamageGive ×0.75`（仅 NORMAL），priority=99
//! - 力量 Strength：造成方 `atDamageGive +amount`（仅 NORMAL），±999 上限，可负
//! - 脆弱 Frail：受击方格挡 `modifyBlock ×0.75`，priority=10
//! - 回合衰减（atEndOfRound）：易伤/虚弱/脆弱 isTurnBased，每轮 -1；
//!   怪物施加的（justApplied）跳过第一次衰减（易伤额外要求 turnHasEnded）

/// DamageInfo.DamageType：NORMAL 走乘区，THORNS/HP_LOSS 不走
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageType {
    Normal,
    Thorns,
    HpLoss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerKind {
    Strength,
    Vulnerable,
    Weak,
    Frail,
    /// 仪式（RitualPower）：每轮末投予力量；怪物侧首轮 skipFirst
    Ritual,
    /// 活动肌肉的回合末力量失效（LoseStrengthPower：EOT 力量 -amount 后移除自身）
    LoseStrength,
    /// 敏捷（DexterityPower：格挡 +flat，±999，可负）
    Dexterity,
    /// 中毒（PoisonPower：怪 EOT 失 HP=层数然后 -1，封顶 9999，绕过格挡）
    Poison,
    /// 金属化（MetallicizePower：玩家 EOT 前 GainBlock(amount)）
    Metallicize,
    /// 再生（Regeneration：EOT 治疗 amount 后 -1 层）
    Regen,
}

/// atEndOfRound 衰减结果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decay {
    /// justApplied 消耗，本轮不减
    SkipFirst,
    /// 仍在（如 Strength 无衰减 / 层数未归零）
    Stay,
    /// 层数-1 后仍存在
    Reduced(i32),
    /// 移除（归零或本来就是 0）
    Remove,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Power {
    pub kind: PowerKind,
    pub amount: i32,
    pub just_applied: bool,
}

impl Power {
    pub fn new(
        kind: PowerKind,
        amount: i32,
        source_is_monster: bool,
        turn_has_ended: bool,
    ) -> Self {
        let just_applied = match kind {
            // VulnerablePower: if (turnHasEnded && isSourceMonster)
            PowerKind::Vulnerable => source_is_monster && turn_has_ended,
            // WeakPower / FrailPower: if (isSourceMonster)
            PowerKind::Weak | PowerKind::Frail => source_is_monster,
            // RitualPower: skipFirst = true（怪物侧首轮不投予）；其余无 skip
            PowerKind::Ritual => true,
            PowerKind::LoseStrength
            | PowerKind::Strength
            | PowerKind::Dexterity
            | PowerKind::Poison
            | PowerKind::Metallicize
            | PowerKind::Regen => false,
        };
        Self {
            kind,
            amount,
            just_applied,
        }
    }

    pub fn id(&self) -> &'static str {
        match self.kind {
            PowerKind::Strength => "Strength",
            PowerKind::Vulnerable => "Vulnerable",
            PowerKind::Weak => "Weakened",
            PowerKind::Frail => "Frail",
            PowerKind::Ritual => "Ritual",
            PowerKind::LoseStrength => "LoseStrengthPower",
            PowerKind::Dexterity => "Dexterity",
            PowerKind::Poison => "Poison",
            PowerKind::Metallicize => "Metallicize",
            PowerKind::Regen => "Regeneration",
        }
    }

    /// 是否走通用轮末衰减（易伤/虚弱/脆弱）；毒/再生有自己的 tick，仪式/金属化不衰减
    pub fn is_turn_based(&self) -> bool {
        matches!(
            self.kind,
            PowerKind::Vulnerable | PowerKind::Weak | PowerKind::Frail
        )
    }

    /// atDamageGive：造成伤害方的 power（StrengthPower: +flat；WeakPower: ×0.75）
    pub fn at_damage_give(&self, dmg: f32, ty: DamageType) -> f32 {
        match (self.kind, ty) {
            (PowerKind::Strength, DamageType::Normal) => dmg + self.amount as f32,
            (PowerKind::Weak, DamageType::Normal) => dmg * 0.75,
            _ => dmg,
        }
    }

    /// atDamageReceive：受击方的 power（VulnerablePower: ×1.5）
    pub fn at_damage_receive(&self, dmg: f32, ty: DamageType) -> f32 {
        match (self.kind, ty) {
            (PowerKind::Vulnerable, DamageType::Normal) => dmg * 1.5,
            _ => dmg,
        }
    }

    /// atDamageFinalGive/FinalReceive：M2 无实现者，保留钩子位
    pub fn at_damage_final_give(&self, dmg: f32, _ty: DamageType) -> f32 {
        dmg
    }

    pub fn at_damage_final_receive(&self, dmg: f32, _ty: DamageType) -> f32 {
        dmg
    }

    /// modifyBlock（FrailPower: ×0.75；在受格挡方的 power 列表上迭代）
    pub fn modify_block(&self, block: f32) -> f32 {
        match self.kind {
            PowerKind::Frail => block * 0.75,
            PowerKind::Dexterity => block + self.amount as f32,
            _ => block,
        }
    }

    /// atEndOfRound：按源码 `justApplied → amount==0 ? Remove : Reduce(1)`，
    /// ReducePowerAction 归零即移除。Strength 无衰减。
    pub fn at_end_of_round(&mut self) -> Decay {
        if !self.is_turn_based() {
            return Decay::Stay;
        }
        if self.just_applied {
            self.just_applied = false;
            return Decay::SkipFirst;
        }
        if self.amount == 0 {
            return Decay::Remove;
        }
        self.amount -= 1;
        if self.amount == 0 {
            Decay::Remove
        } else {
            Decay::Reduced(self.amount)
        }
    }
}

/// ApplyPowerAction + stackPower 语义：同类叠加；Strength ±999 上限且归零移除
pub fn apply_power(
    target: &mut Vec<Power>,
    kind: PowerKind,
    amount: i32,
    source_is_monster: bool,
    turn_has_ended: bool,
) {
    if let Some(p) = target.iter_mut().find(|p| p.kind == kind) {
        p.amount += amount;
        if matches!(kind, PowerKind::Strength) {
            p.amount = p.amount.clamp(-999, 999);
            if p.amount == 0 {
                target.retain(|q| q.kind != kind);
            }
        }
    } else {
        let mut p = Power::new(kind, amount, source_is_monster, turn_has_ended);
        if matches!(kind, PowerKind::Strength) {
            p.amount = p.amount.clamp(-999, 999);
        }
        target.push(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与游戏数值常识对拍：floor(6×1.5)=9、floor(11×0.75)=8、floor(5×0.75)=3
    #[test]
    fn multipliers_exact() {
        let vuln = Power::new(PowerKind::Vulnerable, 2, false, false);
        assert_eq!(vuln.at_damage_receive(6.0, DamageType::Normal), 9.0);
        // THORNS/HP_LOSS 不吃乘区
        assert_eq!(vuln.at_damage_receive(6.0, DamageType::Thorns), 6.0);

        let weak = Power::new(PowerKind::Weak, 1, false, false);
        assert_eq!(weak.at_damage_give(11.0, DamageType::Normal), 8.25);

        let frail = Power::new(PowerKind::Frail, 1, false, false);
        assert_eq!(frail.modify_block(5.0), 3.75);

        let str_ = Power::new(PowerKind::Strength, 3, false, false);
        assert_eq!(str_.at_damage_give(11.0, DamageType::Normal), 14.0);
        assert_eq!(str_.at_damage_give(11.0, DamageType::HpLoss), 11.0);
    }

    /// Bash 式衰减：施加 2 层 → 第一轮末 1 → 第二轮末移除
    #[test]
    fn vulnerable_decay_player_source() {
        let mut p = Power::new(PowerKind::Vulnerable, 2, false, false);
        assert_eq!(p.at_end_of_round(), Decay::Reduced(1));
        assert_eq!(p.at_end_of_round(), Decay::Remove);
    }

    /// 怪物施加的虚弱：跳过第一次衰减（WeakPower.justApplied）
    #[test]
    fn weak_decay_monster_source() {
        let mut p = Power::new(PowerKind::Weak, 1, true, true);
        assert_eq!(p.at_end_of_round(), Decay::SkipFirst);
        assert_eq!(p.at_end_of_round(), Decay::Remove);
    }

    /// Strength 叠加 ±999 上限、归零移除（stackPower 源码）
    #[test]
    fn strength_stack_semantics() {
        let mut v = vec![];
        apply_power(&mut v, PowerKind::Strength, 3, false, false);
        apply_power(&mut v, PowerKind::Strength, -3, false, false);
        assert!(v.is_empty());
        apply_power(&mut v, PowerKind::Strength, 1000, false, false);
        assert_eq!(v[0].amount, 999);
    }
}
