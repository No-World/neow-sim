//! M2 战斗状态机：铁甲战士（打击/防御/痛击）vs 颚虫。
//!
//! 执行序对照 docs/engine-semantics.md：
//! - 出牌：calculateCardDamage → use() 入队效果 → UseCardAction 去向（手→弃）→ 扣能量
//! - 回合结束：整手弃牌 → 怪物 pre-turn 掉格挡 → takeTurn 入队（末尾 RollMove）
//!   → applyEndOfTurnPowers（玩家 powers 衰减 → 怪物 powers 衰减）
//!   → 下回合：turn++ → 玩家掉格挡 → 能量重置 → 抽 5
//! - 意图决策：aiRng.random(99) 主掷 + 招内 randomBoolean(p)，与 JawWorm.getMove 逐行一致

use std::hash::{Hash, Hasher};

use crate::cards::{ironclad_starter, CardKind};
use crate::damage::{calc_monster_damage, calc_player_card_block, calc_player_card_damage};
use crate::powers::{apply_power, DamageType, Decay, Power, PowerKind};
use crate::rng::{card_group_shuffle, RngStreams};
use crate::state::{Creature, JawWormState, PlayerState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    PlayCard(usize),
    EndTurn,
}

/// 引擎级原子动作（对应引擎 action 队列的 M2 子集）
#[derive(Clone, Copy, Debug)]
enum Q {
    /// 玩家卡伤害（已过 calculateCardDamage 管线）
    DamageMonster(i32),
    /// 怪物伤害（结算时过 DamageInfo.applyPowers 管线）
    MonsterDamage(i32),
    PlayerBlock(i32),
    MonsterBlock(i32),
    WormStrength(i32),
    WormVulnerable(i32),
    RollMove,
}

pub struct Combat {
    pub player: PlayerState,
    pub worm: JawWormState,
    pub rng: RngStreams,
    pub turn: i32,
    pub turn_has_ended: bool,
    pub over: bool,
    pub won: bool,
    pub ascension: i32,
}

impl Combat {
    pub fn new(seed: u64, ascension: i32) -> Self {
        let (chomp, thrash_d, thrash_b, b_str, b_block, (hp_min, hp_max)) =
            JawWormState::stats(ascension);
        let mut rng = RngStreams::new(seed);
        // setHp：monsterHpRng.random(min, max)
        let hp = rng.monster_hp.random_start_end(hp_min, hp_max);
        let mut c = Combat {
            player: PlayerState {
                creature: Creature::new(80, 80), // TODO(M3): 进阶玩家侧修正
                energy: 3,
                hand: Vec::new(),
                draw: ironclad_starter(ascension),
                discard: Vec::new(),
                exhaust: Vec::new(),
            },
            worm: JawWormState {
                creature: Creature::new(hp, hp),
                next_move: 1,
                move_history: Vec::new(),
                first_move: true,
                chomp_dmg: chomp,
                thrash_dmg: thrash_d,
                thrash_block: thrash_b,
                bellow_str: b_str,
                bellow_block: b_block,
            },
            rng,
            turn: 1,
            turn_has_ended: false,
            over: false,
            won: false,
            ascension,
        };
        // 战斗开始：洗抽牌堆（shuffleRng）→ 抽 5 → 首个意图（消耗 1 次 aiRng）
        card_group_shuffle(&mut c.player.draw, &mut c.rng.shuffle);
        c.draw_cards(5);
        c.roll_move();
        c
    }

    pub fn legal_actions(&self) -> Vec<Action> {
        let mut v = Vec::new();
        if self.over {
            return v;
        }
        for (i, card) in self.player.hand.iter().enumerate() {
            if card.playable() && card.cost() <= self.player.energy {
                v.push(Action::PlayCard(i));
            }
        }
        v.push(Action::EndTurn);
        v
    }

    /// 意图显示伤害（createIntent → calculateDamage 管线，随 power 变化）
    pub fn intent_damage(&self) -> Option<i32> {
        self.worm.move_base_damage().map(|base| {
            calc_monster_damage(
                base,
                DamageType::Normal,
                &self.worm.creature.powers,
                &self.player.creature.powers,
            )
        })
    }

    pub fn step(&mut self, a: &Action) {
        if self.over {
            return;
        }
        match a {
            Action::PlayCard(i) => self.play_card(*i),
            Action::EndTurn => self.end_turn(),
        }
    }

    fn play_card(&mut self, i: usize) {
        let card = self.player.hand[i];
        // 1) calculateCardDamage(monster)：出牌前按当前双方 power 计算
        let mut q: Vec<Q> = Vec::new();
        match card.kind {
            CardKind::Strike => {
                let d = calc_player_card_damage(
                    card.base_damage().unwrap(),
                    DamageType::Normal,
                    &self.player.creature.powers,
                    &self.worm.creature.powers,
                );
                q.push(Q::DamageMonster(d));
            }
            CardKind::Bash => {
                let d = calc_player_card_damage(
                    card.base_damage().unwrap(),
                    DamageType::Normal,
                    &self.player.creature.powers,
                    &self.worm.creature.powers,
                );
                q.push(Q::DamageMonster(d));
                q.push(Q::WormVulnerable(card.magic().unwrap()));
            }
            CardKind::Defend => {
                let b = calc_player_card_block(
                    card.base_block().unwrap(),
                    &self.player.creature.powers,
                );
                q.push(Q::PlayerBlock(b));
            }
            CardKind::AscendersBane => unreachable!("Ascender's Bane 不可打出"),
        }
        // 2) UseCardAction 去向：手牌 → 弃牌堆
        self.player.hand.remove(i);
        self.player.discard.push(card);
        // 3) 能量结算
        self.player.energy -= card.cost();
        self.drain(q);
    }

    fn end_turn(&mut self) {
        self.turn_has_ended = true;
        // DiscardAtEndOfTurn：无保留卡（retain）与 Runic Pyramid/Equilibrium 例外
        let hand = std::mem::take(&mut self.player.hand);
        self.player.discard.extend(hand);
        // 怪物 pre-turn 掉格挡（MonsterGroup.applyPreTurnLogic；调用点待 L1 对拍核对）
        self.worm.creature.block = 0;
        // takeTurn：按 next_move 入队，末尾 RollMove（下回合意图此时掷出）
        let mut q: Vec<Q> = Vec::new();
        match self.worm.next_move {
            1 => q.push(Q::MonsterDamage(self.worm.chomp_dmg)),
            2 => {
                q.push(Q::WormStrength(self.worm.bellow_str));
                q.push(Q::MonsterBlock(self.worm.bellow_block));
            }
            3 => {
                q.push(Q::MonsterDamage(self.worm.thrash_dmg));
                q.push(Q::MonsterBlock(self.worm.thrash_block));
            }
            _ => {}
        }
        q.push(Q::RollMove);
        self.drain(q);
        if self.over {
            return;
        }
        // applyEndOfTurnPowers：先玩家 powers atEndOfRound，后怪物 powers
        decay_powers(&mut self.player.creature.powers);
        decay_powers(&mut self.worm.creature.powers);
        // 下回合开始序列
        self.turn += 1;
        self.turn_has_ended = false;
        self.player.creature.block = 0; // loseBlock（Barricade/Blur/Calipers 例外不适用）
        self.player.energy = 3;
        self.draw_cards(5);
    }

    fn drain(&mut self, mut q: Vec<Q>) {
        while !q.is_empty() {
            let act = q.remove(0);
            self.exec(act);
            if self.over {
                return;
            }
        }
    }

    fn exec(&mut self, a: Q) {
        match a {
            Q::DamageMonster(d) => {
                self.worm.creature.apply_damage(d);
                if self.worm.creature.is_dead() {
                    self.over = true;
                    self.won = true;
                }
            }
            Q::MonsterDamage(base) => {
                let d = calc_monster_damage(
                    base,
                    DamageType::Normal,
                    &self.worm.creature.powers,
                    &self.player.creature.powers,
                );
                self.player.creature.apply_damage(d);
                if self.player.creature.is_dead() {
                    self.over = true;
                    self.won = false;
                }
            }
            Q::PlayerBlock(b) => self.player.creature.block += b,
            Q::MonsterBlock(b) => self.worm.creature.block += b,
            Q::WormStrength(n) => apply_power(
                &mut self.worm.creature.powers,
                PowerKind::Strength,
                n,
                false,
                self.turn_has_ended,
            ),
            // Bash 施加的易伤：isSourceMonster=false（justApplied=false）
            Q::WormVulnerable(n) => apply_power(
                &mut self.worm.creature.powers,
                PowerKind::Vulnerable,
                n,
                false,
                self.turn_has_ended,
            ),
            Q::RollMove => self.roll_move(),
        }
    }

    /// DrawCardAction + EmptyDeckShuffleAction：抽牌堆空 → 弃牌堆洗回（shuffleRng）
    pub fn draw_cards(&mut self, n: usize) {
        for _ in 0..n {
            if self.player.draw.is_empty() {
                if self.player.discard.is_empty() {
                    break;
                }
                let mut pile = std::mem::take(&mut self.player.discard);
                card_group_shuffle(&mut pile, &mut self.rng.shuffle);
                self.player.draw = pile;
            }
            let c = self.player.draw.pop().unwrap();
            if self.player.hand.len() >= 10 {
                // 手牌上限：直接进弃牌堆
                self.player.discard.push(c);
            } else {
                self.player.hand.push(c);
            }
        }
    }

    /// JawWorm.getMove 逐行复刻（num = aiRng.random(99)）
    fn roll_move(&mut self) {
        let num = self.rng.ai.random_range(99);
        let mv: u8;
        if self.worm.first_move {
            self.worm.first_move = false;
            mv = 1; // 首招恒为咬噬
        } else if num < 25 {
            let last_chomp = self.worm.move_history.last() == Some(&1);
            mv = if last_chomp {
                if self.rng.ai.random_boolean_chance(0.5625) {
                    2
                } else {
                    3
                }
            } else {
                1
            };
        } else if num < 55 {
            let h = &self.worm.move_history;
            let last2_thrash = h.len() >= 2 && h[h.len() - 1] == 3 && h[h.len() - 2] == 3;
            mv = if last2_thrash {
                if self.rng.ai.random_boolean_chance(0.357) {
                    1
                } else {
                    2
                }
            } else {
                3
            };
        } else {
            let last_bellow = self.worm.move_history.last() == Some(&2);
            mv = if last_bellow {
                if self.rng.ai.random_boolean_chance(0.416) {
                    1
                } else {
                    3
                }
            } else {
                2
            };
        }
        self.worm.move_history.push(mv);
        self.worm.next_move = mv;
    }

    /// 规则状态稳定哈希（parity-spec §4 的 M2 子集）
    pub fn state_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.player.creature.hp.hash(&mut h);
        self.player.creature.block.hash(&mut h);
        self.player.energy.hash(&mut h);
        for c in &self.player.hand {
            (c.kind, c.upgraded).hash(&mut h);
        }
        for c in &self.player.draw {
            (c.kind, c.upgraded).hash(&mut h);
        }
        for c in &self.player.discard {
            (c.kind, c.upgraded).hash(&mut h);
        }
        self.worm.creature.hp.hash(&mut h);
        self.worm.creature.block.hash(&mut h);
        self.worm.next_move.hash(&mut h);
        self.worm.move_history.hash(&mut h);
        for p in &self.worm.creature.powers {
            (p.id(), p.amount).hash(&mut h);
        }
        self.turn.hash(&mut h);
        self.rng.ai.counter.hash(&mut h);
        self.rng.shuffle.counter.hash(&mut h);
        self.rng.monster_hp.counter.hash(&mut h);
        h.finish()
    }
}

/// atEndOfRound 衰减（Remove 则移除）
fn decay_powers(powers: &mut Vec<Power>) {
    let mut out = Vec::with_capacity(powers.len());
    for mut p in powers.drain(..) {
        if !matches!(p.at_end_of_round(), Decay::Remove) {
            out.push(p);
        }
    }
    *powers = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::{CardInst, CardKind};
    use crate::powers::{Power, PowerKind};

    /// 测试辅助：强制指定手牌（原手牌放回抽牌堆底）
    fn force_hand(c: &mut Combat, cards: &[CardInst]) {
        let old = std::mem::take(&mut c.player.hand);
        c.player.draw.extend(old);
        c.player.hand = cards.to_vec();
    }

    #[test]
    fn bash_vulnerable_then_strike() {
        let mut c = Combat::new(42, 0);
        let hp0 = c.worm.creature.hp;
        // T1：痛击（2费）→ 8 伤 + 2 层易伤
        force_hand(&mut c, &[CardInst::new(CardKind::Bash)]);
        c.step(&Action::PlayCard(0));
        assert_eq!(c.worm.creature.hp, hp0 - 8);
        assert_eq!(
            c.worm.creature.powers[0],
            Power {
                kind: PowerKind::Vulnerable,
                amount: 2,
                just_applied: false
            }
        );
        // T1 结束：颚虫咬噬 11 → 玩家 69；轮末易伤 2→1
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, 80 - 11);
        assert_eq!(c.worm.creature.powers[0].amount, 1);
        // T2：打击打易伤颚虫 floor(6×1.5)=9
        force_hand(&mut c, &[CardInst::new(CardKind::Strike)]);
        c.step(&Action::PlayCard(0));
        assert_eq!(c.worm.creature.hp, hp0 - 8 - 9);
        // T2 结束 → 轮末易伤 1→0 移除
        c.step(&Action::EndTurn);
        assert!(c.worm.creature.powers.is_empty());
    }

    #[test]
    fn bellow_grants_strength_and_blocks_chomp() {
        let mut c = Combat::new(1, 0);
        // 强制 T1 后意图为低吼（模拟 roll 出 2）
        c.worm.next_move = 2;
        c.step(&Action::EndTurn);
        let str_amt = c.worm.bellow_str;
        assert_eq!(c.worm.creature.powers[0].amount, str_amt);
        assert_eq!(c.worm.creature.block, c.worm.bellow_block);
        // 下一轮咬噬带力量：floor(11 + str)，意图伤害同样体现
        c.worm.next_move = 1;
        assert_eq!(c.intent_damage(), Some(11 + str_amt));
        let hp_before = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp_before - (11 + str_amt));
    }

    #[test]
    fn frail_reduces_defend() {
        let mut c = Combat::new(3, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Frail,
            2,
            true,
            true,
        );
        force_hand(&mut c, &[CardInst::new(CardKind::Defend)]);
        c.step(&Action::PlayCard(0));
        assert_eq!(c.player.creature.block, 3); // floor(5×0.75)
    }

    /// 战斗可完整打完，且同种子同策略轨迹逐点一致；异种子轨迹不同
    #[test]
    fn battle_completes_and_replay_identical() {
        let run = |seed: u64| -> (Vec<u64>, bool, i32) {
            let mut c = Combat::new(seed, 0);
            let mut trace = vec![c.state_hash()];
            for _ in 0..100 {
                if c.over {
                    break;
                }
                // 策略：按手牌顺序尽量出牌
                while let Some(Action::PlayCard(i)) = c
                    .legal_actions()
                    .into_iter()
                    .find(|a| matches!(a, Action::PlayCard(_)))
                {
                    c.step(&Action::PlayCard(i));
                    trace.push(c.state_hash());
                    if c.over {
                        break;
                    }
                }
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
                trace.push(c.state_hash());
            }
            (trace, c.won, c.turn)
        };
        let (t1, won1, turns1) = run(7);
        let (t2, won2, turns2) = run(7);
        assert_eq!(t1, t2, "同种子轨迹必须逐点一致");
        assert_eq!(won1, won2);
        assert_eq!(turns1, turns2);
        assert!(t1.len() < 200, "100 步内必须分出胜负");
        let (t3, _, _) = run(8);
        assert_ne!(t1, t3, "异种子轨迹应不同（概率性断言，种子固定不会抖动）");
    }

    /// 牌堆耗尽 → 弃牌堆洗回（消耗 1 次 shuffle 流调用）
    #[test]
    fn reshuffle_when_draw_empty() {
        let mut c = Combat::new(5, 0);
        c.player.draw.clear();
        c.player.hand.clear();
        c.player.discard = vec![
            CardInst::new(CardKind::Strike),
            CardInst::new(CardKind::Defend),
        ];
        let before = c.rng.shuffle.counter;
        c.draw_cards(5);
        assert_eq!(c.rng.shuffle.counter, before + 1);
        assert_eq!(c.player.hand.len(), 2);
    }
}
