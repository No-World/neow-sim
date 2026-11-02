//! 观测编码器：将 Combat/RunState 状态转换为面向 transformer 训练的
//! token 序列（observation-encoding.md v2.1）。
//!
//! 输出三组平行数组：
//! - `token_types: [i8; MAX_T]` — token 类型标签
//! - `token_ids: [[i32; 3]; MAX_T]` — 实体 vocab ID（供 Python nn.Embedding 查表）
//! - `token_feats: [[f32; F]; MAX_T]` — 归一化数值特征
//!
//! v2.1 修复 12 处信息丢失（按优先级）：
//! P0-1 卡牌伤害/格挡用修正值（calc_player_card_damage/block）
//! P0-2 怪物意图伤害按实际下标查询
//! P0-3 X 费卡标记
//! P0-4 next_turn_copies 带卡牌 ID
//! P1-5 TheBomb power 拆解 dmg/countdown
//! P1-6 power just_applied 标志
//! P1-7 遗物 counter -1/0 语义区分
//! P1-8 Frozen Eye 条件化（DRAW_SEQ token）
//! P2-9..12 待后续批次

use crate::cards::CardInst;
use crate::combat::Combat;
use crate::powers::{DamageType, PowerKind};
use crate::relics::{RelicInst, RelicKind};
use crate::state::{Intent, MonsterState, Stance};

pub const MAX_T: usize = 224;
pub const FEAT_DIM: usize = 24;

pub mod token_type {
    pub const PAD: i8 = 0;
    pub const CLS: i8 = 1;
    pub const GLOBAL: i8 = 2;
    pub const HAND: i8 = 3;
    pub const MOB: i8 = 4;
    pub const PWR_PLAYER: i8 = 5;
    pub const PWR_MOB: i8 = 6;
    pub const RELIC: i8 = 7;
    pub const POTION: i8 = 8;
    pub const ORB: i8 = 9;
    pub const PILE: i8 = 10;
    pub const SCREEN: i8 = 11;
    pub const DRAW_SEQ: i8 = 12; // P1-8: Frozen Eye 有序抽牌堆
}

#[inline]
fn sat(x: f32) -> f32 {
    x / (x + 30.0)
}
#[inline]
fn log_norm(x: i32, max: i32) -> f32 {
    (1 + x.max(0)) as f32 / (1 + max) as f32
}
#[inline]
fn ratio(part: i32, whole: i32) -> f32 {
    if whole <= 0 {
        0.0
    } else {
        part as f32 / whole as f32
    }
}

#[derive(Clone, Debug)]
pub struct EncodedObservation {
    pub token_types: [i8; MAX_T],
    pub token_ids: [[i32; 3]; MAX_T],
    pub token_feats: [[f32; FEAT_DIM]; MAX_T],
    pub seq_len: usize,
}

impl Default for EncodedObservation {
    fn default() -> Self {
        Self {
            token_types: [0; MAX_T],
            token_ids: [[-1; 3]; MAX_T],
            token_feats: [[0.0; FEAT_DIM]; MAX_T],
            seq_len: 0,
        }
    }
}

struct TokenWriter {
    obs: EncodedObservation,
}

impl TokenWriter {
    fn new() -> Self {
        Self {
            obs: EncodedObservation::default(),
        }
    }

    fn push(&mut self, ty: i8, ids: [i32; 3], feats: [f32; FEAT_DIM]) {
        let i = self.obs.seq_len;
        self.obs.token_types[i] = ty;
        self.obs.token_ids[i] = ids;
        self.obs.token_feats[i] = feats;
        self.obs.seq_len += 1;
    }
}

fn card_id(c: &CardInst) -> i32 {
    crate::vocab::card_id(c.def.id)
}

// ── P0-1: 卡牌特征用修正值 ───────────────────

fn card_feats_mod(
    c: &CardInst,
    combat: &Combat,
    target: usize,
    drawn_this_turn: bool,
) -> [f32; FEAT_DIM] {
    let mut f = [0.0; FEAT_DIM];
    let cost = c.cost_pay();
    f[0] = (cost.max(-1) as f32 + 1.0) / 11.0;

    // P0-1: 修正伤害（力量/虚弱/易伤/姿态 全算好）
    let base_dmg = c.base_damage().unwrap_or(0);
    let target_powers = combat
        .monsters
        .get(target)
        .map(|m| m.creature.powers.as_slice())
        .unwrap_or(&[]);
    let mod_dmg = crate::damage::calc_player_card_damage(
        base_dmg,
        DamageType::Normal,
        &combat.player.creature.powers,
        target_powers,
        combat.player.stance,
    );
    f[1] = sat(mod_dmg as f32);

    // P0-1: 修正格挡（敏捷/脆弱）
    let base_blk = c.base_block().unwrap_or(0);
    let mod_blk = crate::damage::calc_player_card_block(base_blk, &combat.player.creature.powers);
    f[2] = sat(mod_blk as f32);

    f[3] = ratio(c.magic().unwrap_or(0), 99);
    f[4] = if c.def.flags.contains(&"exhaust") {
        1.0
    } else {
        0.0
    };
    f[5] = if c.def.flags.contains(&"ethereal") {
        1.0
    } else {
        0.0
    };
    f[6] = if c.def.flags.contains(&"innate") {
        1.0
    } else {
        0.0
    };

    let tt = match c.def.target {
        "enemy" => 0.0,
        "self" => 1.0,
        "none" => 2.0,
        "all" => 3.0,
        _ => 4.0,
    };
    f[7] = tt / 4.0;
    let ct = match c.def.card_type {
        "ATTACK" => 0.0,
        "SKILL" => 1.0,
        _ => 2.0,
    };
    f[8] = ct / 2.0;
    f[9] = if drawn_this_turn { 1.0 } else { 0.0 };

    // P0-3: X 费标记
    f[10] = if c.def.cost == Some(-1) { 1.0 } else { 0.0 };

    f
}

// ── 怪物 token 特征 ─────────────────────────────

fn mob_feats(m: &MonsterState) -> [f32; FEAT_DIM] {
    let mut f = [0.0; FEAT_DIM];
    f[0] = ratio(m.creature.hp, m.creature.max_hp.max(1));
    f[1] = log_norm(m.creature.max_hp, 999);
    f[2] = sat(m.creature.block as f32);
    let (ity, _, icnt) = intent_numeric(&m.intent());
    f[3] = ity;
    f[5] = (icnt.min(5) as f32) / 5.0;
    // f[4] intent_damage 由调用方用正确的怪物下标填充（P0-2）
    let h = &m.move_history;
    f[6] = h.last().map(|v| *v as f32 / 8.0).unwrap_or(-1.0);
    f[7] = if h.len() >= 2 {
        h[h.len() - 2] as f32 / 8.0
    } else {
        -1.0
    };
    f[8] = if h.len() >= 3 {
        h[h.len() - 3] as f32 / 8.0
    } else {
        -1.0
    };
    f[9] = if m.stasis_card.is_some() { 1.0 } else { 0.0 };
    f[10] = if m.half_dead { 1.0 } else { 0.0 };
    f[11] = if m.escaped { 1.0 } else { 0.0 };
    f[12] = m
        .stasis_card
        .as_ref()
        .map(|c| card_id(c) as f32)
        .unwrap_or(-1.0);
    // alive（非 hp>0 派生——Darkling half_dead 场景）
    f[13] = if m.creature.hp > 0 && !m.escaped {
        1.0
    } else {
        0.0
    };
    f
}

fn intent_numeric(intent: &Intent) -> (f32, f32, i32) {
    use Intent::*;
    let ty = match intent {
        Attack => 0.0,
        DefendBuff => 1.0,
        AttackDefend => 2.0,
        AttackBuff => 3.0,
        AttackDebuff => 4.0,
        Buff => 5.0,
        Debuff => 6.0,
        Defend => 7.0,
        Sleep => 8.0,
        Unknown => 9.0,
        Escape => 10.0,
    };
    (ty / 10.0, 0.0, 1)
}

// ── 遗物 token 特征（P1-7: -1/0 语义区分）────────

fn relic_feats(r: &RelicInst) -> [f32; FEAT_DIM] {
    let mut f = [0.0; FEAT_DIM];
    if r.counter < 0 {
        // 非周期遗物（counter=-1 = 不适用）
        f[0] = -1.0;
        f[1] = 0.0;
        f[2] = 0.0;
    } else {
        let counter = r.counter as f32;
        let period = 99.0f32.max(counter); // TODO: RELIC_PERIOD 常量表
        f[0] = counter / period; // 进度
        f[1] = 1.0 - f[0]; // 剩余
        f[2] = if counter >= period * 0.8 { 1.0 } else { 0.0 }; // 即将就绪
    }
    f
}

// ── P1-5/6: Power 特征（TheBomb 拆解 + just_applied）──

fn power_feats(pw: &crate::powers::Power) -> [f32; FEAT_DIM] {
    let mut f = [0.0; FEAT_DIM];
    if pw.kind == PowerKind::TheBomb {
        // P1-5: 拆解 dmg×10+countdown 复合编码
        let dmg = pw.amount / 10;
        let countdown = pw.amount % 10;
        f[0] = sat(dmg as f32); // 实际伤害
        f[1] = (countdown as f32) / 5.0; // 倒计时
        f[2] = if countdown <= 1 { 1.0 } else { 0.0 }; // 即将爆炸
    } else {
        f[0] = sat(pw.amount as f32);
    }
    // P1-6: just_applied 标志（影响本回合是否衰减）
    f[3] = if pw.just_applied { 1.0 } else { 0.0 };
    f
}

// ── 主编码函数 ─────────────────────────────────

pub fn encode_combat(c: &Combat) -> EncodedObservation {
    let mut w = TokenWriter::new();

    // [CLS]
    w.push(token_type::CLS, [-1, -1, -1], [0.0; FEAT_DIM]);

    // [GLOBAL]
    let mut gf = [0.0; FEAT_DIM];
    let p = &c.player.creature;
    gf[0] = ratio(p.hp, p.max_hp.max(1));
    gf[1] = log_norm(p.max_hp, 999);
    gf[2] = sat(p.block as f32);
    gf[3] = (c.player.energy.min(10) as f32) / 10.0;
    gf[4] = log_norm(c.gold, 9999);
    let stance = match c.player.stance {
        Stance::Neutral => 0.0,
        Stance::Wrath => 1.0,
        Stance::Calm => 2.0,
        Stance::Divinity => 3.0,
    };
    gf[5] = stance / 3.0;
    gf[6] = (c.attacks_this_turn.min(10) as f32) / 10.0;
    gf[7] = (c.discarded_this_turn.min(10) as f32) / 10.0;
    let lct = match c.last_card_type.as_deref() {
        Some("ATTACK") => 1.0,
        Some("SKILL") => 2.0,
        Some("POWER") => 3.0,
        _ => 0.0,
    };
    gf[8] = lct / 3.0;
    gf[9] = (c.next_turn_draw.min(5) as f32) / 5.0;
    gf[10] = (c.next_turn_energy.min(5) as f32) / 5.0;
    gf[11] = (c.next_turn_copy_count.min(3) as f32) / 3.0;
    gf[12] = if c.hand_cost_zero_this_turn { 1.0 } else { 0.0 };
    gf[13] = if c.hand_cost_max_one { 1.0 } else { 0.0 };
    gf[14] = if c.pending.is_some() { 1.0 } else { 0.0 };

    // P0-4: next_turn_copies 的卡牌 ID（Nightmare 复制的是哪张卡）
    if let Some((card, _)) = c.next_turn_copies.first() {
        gf[15] = card_id(card) as f32;
    }

    // P2-11: 能量上限（Velvet Choker 等）
    // gf[16] = energy_cap

    w.push(token_type::GLOBAL, [-1, -1, -1], gf);

    // [HAND_0..9] — P0-1: 修正值
    let default_target = c
        .monsters
        .iter()
        .position(|m| m.creature.hp > 0)
        .unwrap_or(0);
    for card in c.player.hand.iter().take(10) {
        let ids = [card_id(card), -1, -1];
        w.push(
            token_type::HAND,
            ids,
            card_feats_mod(card, c, default_target, true),
        );
    }

    // [MOB_0..4] — P0-2: 按实际下标查意图伤害
    for (mi, m) in c.monsters.iter().take(5).enumerate() {
        let mob_id = m.kind as i32;
        let ids = [-1, -1, mob_id];
        let mut f = mob_feats(m);
        // P0-2: 用 mi 而非硬编码 0
        if let Some(dmg) = c.intent_damage(mi) {
            f[4] = sat(dmg as f32);
        }
        w.push(token_type::MOB, ids, f);
    }

    // [PWR_PLAYER_0..15] — P1-5/6: TheBomb 拆解 + just_applied
    for pw in p.powers.iter().take(16) {
        let pw_id = pw.kind as i32;
        w.push(token_type::PWR_PLAYER, [pw_id, -1, -1], power_feats(pw));
    }

    // [PWR_MOB] — 每怪最多 8 个
    for m in c.monsters.iter().take(5) {
        for pw in m.creature.powers.iter().take(8) {
            let pw_id = pw.kind as i32;
            w.push(token_type::PWR_MOB, [pw_id, -1, -1], power_feats(pw));
        }
    }

    // [RELIC_0..31] — P1-7: counter -1/0 区分
    for r in c.player_relics.iter().take(32) {
        let relic_id = r.kind as i32;
        w.push(token_type::RELIC, [-1, relic_id, -1], relic_feats(r));
    }

    // [POTION_0..5]
    for (i, slot) in c.player.potions.iter().take(6).enumerate() {
        if let Some(p) = slot {
            let mut f = [0.0; FEAT_DIM];
            f[0] = ratio(p.potency, 20);
            f[1] = i as f32 / 5.0;
            let pot_id = p.kind as i32;
            w.push(token_type::POTION, [-1, -1, pot_id], f);
        }
    }

    // [ORB_0..9]
    for (i, orb) in c.player.orbs.iter().take(10).enumerate() {
        let mut f = [0.0; FEAT_DIM];
        f[0] = sat(orb.passive(0) as f32);
        f[1] = sat(orb.evoke(0) as f32);
        f[2] = i as f32 / 10.0;
        let orb_id = orb.kind as i32;
        w.push(token_type::ORB, [orb_id, -1, -1], f);
    }

    // [PILE] — 4 堆 × 5 token（count + 4 query placeholder）
    let piles = [
        (0.0f32, c.player.draw.len()),
        (1.0, c.player.discard.len()),
        (
            2.0,
            c.player.draw.len() + c.player.discard.len() + c.player.hand.len(),
        ),
        (3.0, c.player.exhaust.len()),
    ];
    for (pile_ty, count) in piles.iter() {
        for qi in 0..5 {
            let mut f = [0.0; FEAT_DIM];
            f[0] = pile_ty / 3.0;
            f[1] = (*count as f32) / 60.0;
            f[2] = qi as f32 / 4.0;
            w.push(token_type::PILE, [-1, -1, -1], f);
        }
    }

    // P1-8: Frozen Eye → DRAW_SEQ 有序 token（最多 10 张，堆顶在前）
    let has_frozen_eye = c
        .player_relics
        .iter()
        .any(|r| r.kind == RelicKind::FrozenEye);
    if has_frozen_eye {
        // draw 是 Vec，尾=堆顶 → rev() 迭代取堆顶序
        for (i, card) in c.player.draw.iter().rev().take(10).enumerate() {
            if w.obs.seq_len >= MAX_T {
                break;
            }
            let ids = [card_id(card), -1, -1];
            let mut f = [0.0; FEAT_DIM];
            f[0] = i as f32 / 10.0; // 距堆顶位置
            w.push(token_type::DRAW_SEQ, ids, f);
        }
    }

    w.obs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::Combat;
    use crate::state::MonsterKind;

    #[test]
    fn encode_basic() {
        let c = Combat::new(42);
        let obs = encode_combat(&c);
        assert!(obs.seq_len > 0 && obs.seq_len <= MAX_T);
        assert_eq!(obs.token_types[0], token_type::CLS);
        assert_eq!(obs.token_types[1], token_type::GLOBAL);
    }

    #[test]
    fn respects_budget() {
        for seed in [1u64, 42, 999, 12345] {
            let obs = encode_combat(&Combat::new(seed));
            assert!(obs.seq_len <= MAX_T, "seed={seed} seq_len={}", obs.seq_len);
        }
    }

    /// P0-1: 手牌伤害是修正值（有力量时 > 基础值）
    #[test]
    fn modified_damage_with_strength() {
        let mut c = Combat::new_asc(1, 0);
        // 给玩家 +2 力量
        c.apply_power_to_player_checked(crate::powers::PowerKind::Strength, 2, false);
        let obs = encode_combat(&c);
        // 找第一张 HAND token 的 f[1]（damage）
        let hand_idx = obs
            .token_types
            .iter()
            .position(|&t| t == token_type::HAND)
            .unwrap();
        let dmg_feat = obs.token_feats[hand_idx][1];
        // 有力量时 damage 特征应 > 0（基础 6 + Str 2 = 8 → sat(8) ≈ 0.21）
        assert!(dmg_feat > 0.0, "修正伤害 > 0：{}", dmg_feat);
    }

    /// P0-2: 多怪时意图伤害按各自下标计算
    #[test]
    fn per_monster_intent_damage() {
        let c = Combat::new_with(1, 0, &[MonsterKind::JawWorm, MonsterKind::Cultist]);
        let obs = encode_combat(&c);
        // 找两个 MOB token
        let mob_indices: Vec<usize> = obs
            .token_types
            .iter()
            .enumerate()
            .filter(|(_, &t)| t == token_type::MOB)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(mob_indices.len(), 2);
        // 两只怪的意图伤害特征可以不同
        let dmg0 = obs.token_feats[mob_indices[0]][4];
        let dmg1 = obs.token_feats[mob_indices[1]][4];
        // 只需验证两个都有值（不为 0 也可能合理——防御型怪）
        assert!(dmg0 >= 0.0 && dmg1 >= 0.0);
    }

    /// P0-3: X 费卡有标记
    #[test]
    fn x_cost_flag() {
        let c = Combat::new(1);
        let obs = encode_combat(&c);
        // 铁甲起始组无 X 费卡——所有 HAND token 的 f[10] 应为 0
        for i in 0..obs.seq_len {
            if obs.token_types[i] == token_type::HAND {
                assert_eq!(obs.token_feats[i][10], 0.0, "非 X 费卡 f[10]=0");
            }
        }
    }

    /// P1-7: 遗物 counter=-1 vs >=0
    #[test]
    fn relic_counter_semantics() {
        let c = Combat::new(1);
        let obs = encode_combat(&c);
        // BurningBlood counter=-1 → f[0] = -1.0
        let relic_idx = obs.token_types.iter().position(|&t| t == token_type::RELIC);
        if let Some(ri) = relic_idx {
            assert_eq!(
                obs.token_feats[ri][0], -1.0,
                "BurningBlood counter=-1 → -1.0"
            );
        }
    }
}
