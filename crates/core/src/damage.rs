//! 伤害/格挡数值管线 —— 逐位复刻（f32 运算 + MathUtils.floor）。
//!
//! 源码对照：
//! - 玩家卡 → 怪物：AbstractCard.calculateCardDamage（单体分支）
//!   `relics.atDamageModify → 玩家powers.atDamageGive → stance.atDamageGive
//!    → 怪物powers.atDamageReceive → 玩家.atDamageFinalGive → 怪物.atDamageFinalReceive
//!    → (tmp<0→0) → floor`
//! - 格挡：applyPowersToBlock `powers.modifyBlock → modifyBlockLast → (tmp<0→0) → floor`
//! - 怪物 → 玩家：DamageInfo.applyPowers(!isPlayer) 与 AbstractMonster.calculateDamage 同序
//!   `怪.atDamageGive → 玩家.atDamageReceive → stance.atDamageReceive
//!    → 怪.atDamageFinalGive → 玩家.atDamageFinalReceive → floor → (out<0→0)`

use crate::powers::{DamageType, Power};
use crate::state::Stance;

/// libgdx MathUtils.floor(float)：(int)Math.floor(value)
pub fn floor(x: f32) -> i32 {
    x.floor() as i32
}

/// AbstractCard.calculateCardDamage（单体、有目标）→ this.damage
pub fn calc_player_card_damage(
    base: i32,
    ty: DamageType,
    player_powers: &[Power],
    monster_powers: &[Power],
    stance: Stance,
) -> i32 {
    let mut tmp = base as f32;
    // relics.atDamageModify：M2 无遗物
    for p in player_powers {
        tmp = p.at_damage_give(tmp, ty);
    }
    // player.stance.atDamageGive（AbstractCard 管线：Give→姿态→Receive）
    tmp = stance.at_damage_give(tmp, ty);
    for p in monster_powers {
        tmp = p.at_damage_receive(tmp, ty);
    }
    for p in player_powers {
        tmp = p.at_damage_final_give(tmp, ty);
    }
    for p in monster_powers {
        tmp = p.at_damage_final_receive(tmp, ty);
    }
    if tmp < 0.0 {
        tmp = 0.0;
    }
    floor(tmp)
}

/// AbstractCard.applyPowersToBlock → this.block
pub fn calc_player_card_block(base: i32, player_powers: &[Power]) -> i32 {
    let mut tmp = base as f32;
    for p in player_powers {
        tmp = p.modify_block(tmp);
    }
    // modifyBlockLast：M2 无实现者
    if tmp < 0.0 {
        tmp = 0.0;
    }
    floor(tmp)
}

/// DamageInfo.applyPowers（!owner.isPlayer，怪物→玩家）与
/// AbstractMonster.calculateDamage（意图显示）共用的管线
pub fn calc_monster_damage(
    base: i32,
    ty: DamageType,
    monster_powers: &[Power],
    player_powers: &[Power],
    stance: Stance,
) -> i32 {
    let mut tmp = base as f32;
    for p in monster_powers {
        tmp = p.at_damage_give(tmp, ty);
    }
    for p in player_powers {
        tmp = p.at_damage_receive(tmp, ty);
    }
    // stance.atDamageReceive（DamageInfo !player 分支：怪Give→玩家Receive→姿态→Final）
    tmp = stance.at_damage_receive(tmp, ty);
    for p in monster_powers {
        tmp = p.at_damage_final_give(tmp, ty);
    }
    for p in player_powers {
        tmp = p.at_damage_final_receive(tmp, ty);
    }
    let out = floor(tmp);
    if out < 0 {
        0
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::powers::{Power, PowerKind};

    #[test]
    fn player_strike_on_vulnerable_worm() {
        // 打击 6 打易伤大颚虫：floor(6×1.5)=9
        let worm = vec![Power::new(PowerKind::Vulnerable, 2, false, false)];
        assert_eq!(
            calc_player_card_damage(6, DamageType::Normal, &[], &worm, Stance::Neutral),
            9
        );
    }

    #[test]
    fn strength_then_vulnerable_order() {
        // 力量 3 的玩家打易伤怪：floor((6+3)×1.5)=13（先加后乘，float 无损）
        let player = vec![Power::new(PowerKind::Strength, 3, false, false)];
        let worm = vec![Power::new(PowerKind::Vulnerable, 2, false, false)];
        assert_eq!(
            calc_player_card_damage(6, DamageType::Normal, &player, &worm, Stance::Neutral),
            13
        );
    }

    #[test]
    fn worm_chomp_with_strength_and_weak_on_attacker() {
        // 虚弱挂在攻击方（大颚虫）才生效：floor((11+3)×0.75)=10；
        // 玩家虚弱不影响受到的伤害（Weak 是 atDamageGive 钩子）
        let worm = vec![
            Power::new(PowerKind::Strength, 3, false, false),
            Power::new(PowerKind::Weak, 1, false, false),
        ];
        assert_eq!(
            calc_monster_damage(11, DamageType::Normal, &worm, &[], Stance::Neutral),
            10
        );
        // 反证：虚弱在玩家身上 → 咬噬不受影响
        let player = vec![Power::new(PowerKind::Weak, 1, true, true)];
        assert_eq!(
            calc_monster_damage(11, DamageType::Normal, &[], &player, Stance::Neutral),
            11
        );
    }

    #[test]
    fn frail_defend() {
        let player = vec![Power::new(PowerKind::Frail, 2, true, true)];
        assert_eq!(calc_player_card_block(5, &player), 3); // floor(5×0.75)=3
    }
}
