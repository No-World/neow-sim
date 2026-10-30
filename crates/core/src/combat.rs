//! M2 战斗状态机：铁甲战士（打击/防御/痛击）vs 怪物组（颚虫/邪教徒）。
//!
//! 执行序对照 docs/engine-semantics.md：
//! - 出牌：calculateCardDamage → use() 入队效果 → UseCardAction 去向（手→弃）→ 扣能量
//! - 回合结束：整手弃牌 → 怪物按列表序：pre-turn 掉格挡 → takeTurn 入队（末尾 RollMove）
//!   → applyEndOfTurnPowers（玩家 powers 衰减 → 逐怪 powers 衰减 + Ritual 投予）
//!   → 下回合：turn++ → 玩家掉格挡 → 能量重置 → 抽 5
//! - 意图决策：aiRng.random(99) 主掷 + 招内 randomBoolean(p)，逐行对照各怪 getMove
//!
//! 诚实型搜索支撑（docs/roadmap.md P4）：
//! - `Clone`：树搜索复制局面
//! - `determinize(world_seed)`：信念采样——抽牌堆未知位置均匀排列 + ai/shuffle 流重播种
//!   （采样出一个"可能世界"，其内部完全确定）
//! - `public_hash()`：信息集哈希（抽牌堆按多重集、不含隐藏计数器），
//!   PIMC 聚合 K 个可能世界的依据

use std::hash::{Hash, Hasher};

use crate::cards::{ironclad_starter, CardKind};
use crate::damage::{calc_monster_damage, calc_player_card_block, calc_player_card_damage};
use crate::powers::{apply_power, DamageType, Decay, Power, PowerKind};
use crate::rng::{card_group_shuffle, java_shuffle, JavaRandom, RngStreams, StsRandom, Xs128};
use crate::state::{Creature, MonsterKind, MonsterState, PlayerState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// 打手牌第 index 张；Attack 卡必须带目标怪物下标
    PlayCard {
        index: usize,
        target: Option<usize>,
    },
    EndTurn,
}

/// 引擎级原子动作（引擎 action 队列的 M2 子集）
#[derive(Clone, Copy, Debug)]
enum Q {
    DamageMonster {
        target: usize,
        dmg: i32,
    },
    MonsterDamage {
        src: usize,
        base: i32,
    },
    PlayerBlock(i32),
    MonsterBlock {
        target: usize,
        amount: i32,
    },
    ApplyPower {
        target: usize,
        kind: PowerKind,
        amount: i32,
    },
    RollMove(usize),
}

#[derive(Clone)]
pub struct Combat {
    pub player: PlayerState,
    pub monsters: Vec<MonsterState>,
    pub rng: RngStreams,
    pub turn: i32,
    pub turn_has_ended: bool,
    pub over: bool,
    pub won: bool,
    pub ascension: i32,
}

impl Combat {
    /// 默认遭遇：单颚虫（M2 基准）
    pub fn new(seed: u64, ascension: i32) -> Self {
        Self::new_with(seed, ascension, &[MonsterKind::JawWorm])
    }

    pub fn new_with(seed: u64, ascension: i32, encounter: &[MonsterKind]) -> Self {
        let mut rng = RngStreams::new(seed);
        let monsters: Vec<MonsterState> = encounter
            .iter()
            .map(|k| MonsterState::spawn(*k, ascension, &mut rng.monster_hp))
            .collect();
        let mut c = Combat {
            player: PlayerState {
                creature: Creature::new(80, 80), // TODO(M3): 进阶玩家侧修正
                energy: 3,
                hand: Vec::new(),
                draw: ironclad_starter(ascension),
                discard: Vec::new(),
                exhaust: Vec::new(),
            },
            monsters,
            rng,
            turn: 1,
            turn_has_ended: false,
            over: false,
            won: false,
            ascension,
        };
        // 战斗开始：洗抽牌堆（shuffleRng）→ 抽 5 → 各怪掷首个意图（每怪消耗 1 次 aiRng）
        card_group_shuffle(&mut c.player.draw, &mut c.rng.shuffle);
        c.draw_cards(5);
        for i in 0..c.monsters.len() {
            c.roll_move(i);
        }
        c
    }

    fn alive(&self, i: usize) -> bool {
        !self.monsters[i].creature.is_dead()
    }

    fn all_monsters_dead(&self) -> bool {
        self.monsters.iter().all(|m| m.creature.is_dead())
    }

    pub fn legal_actions(&self) -> Vec<Action> {
        let mut v = Vec::new();
        if self.over {
            return v;
        }
        for (i, card) in self.player.hand.iter().enumerate() {
            if !card.playable() || card.cost() > self.player.energy {
                continue;
            }
            let needs_target = matches!(card.kind, CardKind::Strike | CardKind::Bash);
            if needs_target {
                for (m, mon) in self.monsters.iter().enumerate() {
                    if !mon.creature.is_dead() {
                        v.push(Action::PlayCard {
                            index: i,
                            target: Some(m),
                        });
                    }
                }
            } else {
                v.push(Action::PlayCard {
                    index: i,
                    target: None,
                });
            }
        }
        v.push(Action::EndTurn);
        v
    }

    /// 意图显示伤害（createIntent → calculateDamage 管线，随 power 变化）
    pub fn intent_damage(&self, m: usize) -> Option<i32> {
        self.monsters[m].move_base_damage().map(|base| {
            calc_monster_damage(
                base,
                DamageType::Normal,
                &self.monsters[m].creature.powers,
                &self.player.creature.powers,
            )
        })
    }

    pub fn step(&mut self, a: &Action) {
        if self.over {
            return;
        }
        match a {
            Action::PlayCard { index, target } => self.play_card(*index, *target),
            Action::EndTurn => self.end_turn(),
        }
    }

    fn play_card(&mut self, i: usize, target: Option<usize>) {
        let card = self.player.hand[i];
        let ti = target.unwrap_or(0);
        let mut q: Vec<Q> = Vec::new();
        match card.kind {
            CardKind::Strike | CardKind::Bash => {
                let d = calc_player_card_damage(
                    card.base_damage().unwrap(),
                    DamageType::Normal,
                    &self.player.creature.powers,
                    &self.monsters[ti].creature.powers,
                );
                q.push(Q::DamageMonster { target: ti, dmg: d });
                if card.kind == CardKind::Bash {
                    q.push(Q::ApplyPower {
                        target: ti,
                        kind: PowerKind::Vulnerable,
                        amount: card.magic().unwrap(),
                    });
                }
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
        // UseCardAction 去向：手牌 → 弃牌堆；能量结算
        self.player.hand.remove(i);
        self.player.discard.push(card);
        self.player.energy -= card.cost();
        self.drain(q);
    }

    fn end_turn(&mut self) {
        self.turn_has_ended = true;
        // DiscardAtEndOfTurn：无保留卡（retain）与 Runic Pyramid/Equilibrium 例外
        let hand = std::mem::take(&mut self.player.hand);
        self.player.discard.extend(hand);
        // 怪物序列（MonsterGroup.queueMonsters 列表序）
        for i in 0..self.monsters.len() {
            if !self.alive(i) {
                continue;
            }
            // pre-turn 掉格挡（MonsterGroup.applyPreTurnLogic；调用点待 L1 对拍核对）
            self.monsters[i].creature.block = 0;
            let mut q = self.monster_turn(i);
            q.push(Q::RollMove(i));
            self.drain(q);
            if self.over {
                return;
            }
        }
        self.end_of_round_powers();
        if self.over {
            return;
        }
        // 下回合开始序列
        self.turn += 1;
        self.turn_has_ended = false;
        self.player.creature.block = 0; // loseBlock（Barricade/Blur/Calipers 例外不适用）
        self.player.energy = 3;
        self.draw_cards(5);
    }

    /// 怪物 takeTurn 入队（演出类动作省略，见 parity-spec 豁免表）
    fn monster_turn(&mut self, i: usize) -> Vec<Q> {
        let m = &self.monsters[i];
        let mut q = Vec::new();
        match m.kind {
            MonsterKind::JawWorm => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => {
                    q.push(Q::ApplyPower {
                        target: i,
                        kind: PowerKind::Strength,
                        amount: m.bellow_str,
                    });
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.bellow_block,
                    });
                }
                3 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_block,
                    });
                }
                _ => {}
            },
            MonsterKind::Cultist => match m.next_move {
                3 => q.push(Q::ApplyPower {
                    target: i,
                    kind: PowerKind::Ritual,
                    amount: m.ritual_amt,
                }),
                _ => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
            },
        }
        q
    }

    /// MonsterGroup.applyEndOfTurnPowers：
    /// 玩家 powers atEndOfRound（衰减）→ 逐怪 powers atEndOfRound（衰减 + Ritual 投予）
    fn end_of_round_powers(&mut self) {
        decay_powers(&mut self.player.creature.powers);
        for i in 0..self.monsters.len() {
            if !self.alive(i) {
                continue;
            }
            // 第一遍：逐 power 结算（衰减 / skipFirst 消耗），收集 Ritual 投予量
            // （Java 中 ApplyPowerAction 入队到底部，即所有 atEndOfRound 之后才结算）
            let mut grants: Vec<i32> = Vec::new();
            for p in &mut self.monsters[i].creature.powers {
                if p.kind == PowerKind::Ritual {
                    if p.just_applied {
                        p.just_applied = false;
                    } else {
                        grants.push(p.amount);
                    }
                }
            }
            decay_powers(&mut self.monsters[i].creature.powers);
            for g in grants {
                apply_power(
                    &mut self.monsters[i].creature.powers,
                    PowerKind::Strength,
                    g,
                    false,
                    self.turn_has_ended,
                );
            }
        }
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
            Q::DamageMonster { target, dmg } => {
                if self.alive(target) {
                    self.monsters[target].creature.apply_damage(dmg);
                    if self.all_monsters_dead() {
                        self.over = true;
                        self.won = true;
                    }
                }
            }
            Q::MonsterDamage { src, base } => {
                let d = calc_monster_damage(
                    base,
                    DamageType::Normal,
                    &self.monsters[src].creature.powers,
                    &self.player.creature.powers,
                );
                self.player.creature.apply_damage(d);
                if self.player.creature.is_dead() {
                    self.over = true;
                    self.won = false;
                }
            }
            Q::PlayerBlock(b) => self.player.creature.block += b,
            Q::MonsterBlock { target, amount } => {
                if self.alive(target) {
                    self.monsters[target].creature.block += amount;
                }
            }
            Q::ApplyPower {
                target,
                kind,
                amount,
            } => {
                // 施加者恒为"对方"视角：怪物对怪物的 STR/Ritual 是 self 源
                apply_power(
                    &mut self.monsters[target].creature.powers,
                    kind,
                    amount,
                    false,
                    self.turn_has_ended,
                );
            }
            Q::RollMove(i) => self.roll_move(i),
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
                self.player.discard.push(c); // 手牌上限：直接进弃牌堆
            } else {
                self.player.hand.push(c);
            }
        }
    }

    /// 意图决策（逐行对照各怪 getMove；num = aiRng.random(99)）
    fn roll_move(&mut self, i: usize) {
        let num = self.rng.ai.random_range(99);
        let mv: u8 = match self.monsters[i].kind {
            MonsterKind::JawWorm => self.jaw_worm_get_move(i, num),
            MonsterKind::Cultist => {
                // Cultist.getMove：首招仪式，之后恒暗击（仍消耗主掷）
                if self.monsters[i].first_move {
                    3
                } else {
                    1
                }
            }
        };
        self.monsters[i].first_move = false;
        self.monsters[i].move_history.push(mv);
        self.monsters[i].next_move = mv;
    }

    /// JawWorm.getMove 逐行复刻
    fn jaw_worm_get_move(&mut self, i: usize, num: i32) -> u8 {
        if self.monsters[i].first_move {
            return 1; // 首招恒为咬噬
        }
        if num < 25 {
            let last_chomp = self.monsters[i].move_history.last() == Some(&1);
            if last_chomp {
                if self.rng.ai.random_boolean_chance(0.5625) {
                    2
                } else {
                    3
                }
            } else {
                1
            }
        } else if num < 55 {
            let h = &self.monsters[i].move_history;
            let last2_thrash = h.len() >= 2 && h[h.len() - 1] == 3 && h[h.len() - 2] == 3;
            if last2_thrash {
                if self.rng.ai.random_boolean_chance(0.357) {
                    1
                } else {
                    2
                }
            } else {
                3
            }
        } else {
            let last_bellow = self.monsters[i].move_history.last() == Some(&2);
            if last_bellow {
                if self.rng.ai.random_boolean_chance(0.416) {
                    1
                } else {
                    3
                }
            } else {
                2
            }
        }
    }

    /// 规则状态稳定哈希（parity-spec §4 的 M2 子集；含隐藏信息，供对拍/复现测试）
    pub fn state_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_public(&mut h);
        for c in &self.player.draw {
            (c.kind, c.upgraded).hash(&mut h); // 真实顺序（隐藏）
        }
        self.rng.ai.counter.hash(&mut h);
        self.rng.shuffle.counter.hash(&mut h);
        self.rng.monster_hp.counter.hash(&mut h);
        h.finish()
    }

    /// 信息集哈希（公开等价类）：抽牌堆按多重集、不含 RNG 计数器。
    /// PIMC 聚合 K 个可能世界、以及搜索中判断"信念等价局面"的依据。
    pub fn public_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_public(&mut h);
        let mut draw_set: Vec<(CardKind, bool, usize)> = self
            .player
            .draw
            .iter()
            .map(|c| (c.kind, c.upgraded, 0usize))
            .collect();
        draw_set.sort();
        draw_set.hash(&mut h);
        h.finish()
    }

    fn hash_public(&self, h: &mut impl Hasher) {
        self.player.creature.hp.hash(h);
        self.player.creature.block.hash(h);
        self.player.energy.hash(h);
        for c in &self.player.hand {
            (c.kind, c.upgraded).hash(h);
        }
        for c in &self.player.discard {
            (c.kind, c.upgraded).hash(h);
        }
        for m in &self.monsters {
            m.creature.hp.hash(h);
            m.creature.block.hash(h);
            m.next_move.hash(h);
            m.move_history.hash(h);
            for p in &m.creature.powers {
                (p.id(), p.amount, p.just_applied).hash(h);
            }
        }
        self.turn.hash(h);
    }

    /// 诚实型信念采样（docs/roadmap.md P4）：
    /// 1. 抽牌堆未知位置 → 均匀随机排列（已知置顶/置底的牌 M2 不存在，M3 需固定）
    /// 2. ai / shuffle 流重播种 = 采样“怪物未来行动 + 未来洗牌”的一条可能轨迹
    ///
    /// 采样后局面内部完全确定，可做确定性推演；belief 采样本身不需要与游戏 RNG 管线对齐。
    pub fn determinize(&mut self, world_seed: u64) {
        let mut r = Xs128::new(world_seed);
        // 均匀洗（Fisher-Yates，本采样与游戏管线无 parity 要求）
        let n = self.player.draw.len();
        for i in (1..n).rev() {
            let j = r.next_long_n(i as u64 + 1) as usize;
            self.player.draw.swap(i, j);
        }
        self.rng.ai = StsRandom::new(r.next_long());
        self.rng.shuffle = StsRandom::new(r.next_long());
    }

    /// 对外暴露一次 java 洗牌（测试/工具用）
    pub fn java_shuffle_slice<T>(v: &mut [T], jr: &mut JavaRandom) {
        java_shuffle(v, jr);
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
    use crate::cards::CardInst;

    /// 测试辅助：强制指定手牌（原手牌放回抽牌堆底）
    fn force_hand(c: &mut Combat, cards: &[CardInst]) {
        let old = std::mem::take(&mut c.player.hand);
        c.player.draw.extend(old);
        c.player.hand = cards.to_vec();
    }

    #[test]
    fn bash_vulnerable_then_strike() {
        let mut c = Combat::new(42, 0);
        let hp0 = c.monsters[0].creature.hp;
        force_hand(&mut c, &[CardInst::new(CardKind::Bash)]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 8);
        assert_eq!(c.monsters[0].creature.powers[0].amount, 2); // 2 层易伤
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, 80 - 11); // 咬噬 11
        assert_eq!(c.monsters[0].creature.powers[0].amount, 1); // 轮末 2→1
        force_hand(&mut c, &[CardInst::new(CardKind::Strike)]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 8 - 9); // floor(6×1.5)=9
        c.step(&Action::EndTurn);
        assert!(c.monsters[0].creature.powers.is_empty()); // 1→0 移除
    }

    #[test]
    fn bellow_grants_strength_and_blocks_chomp() {
        let mut c = Combat::new(1, 0);
        c.monsters[0].next_move = 2; // 强制低吼
        c.step(&Action::EndTurn);
        let str_amt = c.monsters[0].bellow_str;
        assert_eq!(c.monsters[0].creature.powers[0].amount, str_amt);
        assert_eq!(c.monsters[0].creature.block, c.monsters[0].bellow_block);
        c.monsters[0].next_move = 1; // 强制咬噬
        assert_eq!(c.intent_damage(0), Some(11 + str_amt));
        let hp_before = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp_before - (11 + str_amt));
    }

    /// 邪教徒：T1 仪式（当轮不涨力量）→ T2 暗击 6 → 轮末 +3 力 → T3 暗击 9
    #[test]
    fn cultist_ritual_growth() {
        let mut c = Combat::new_with(11, 0, &[MonsterKind::Cultist]);
        assert!(c.monsters[0].creature.max_hp >= 48 && c.monsters[0].creature.max_hp <= 54);
        // T1：仪式（skipFirst 消耗，尚无力量）
        c.step(&Action::EndTurn);
        let ritual = c.monsters[0].creature.powers[0].clone();
        assert_eq!(ritual.kind, PowerKind::Ritual);
        assert!(!c.monsters[0]
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Strength));
        // T2：暗击 6
        let hp1 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp1 - 6);
        // 轮末 +3 力 → T3 暗击 6+3=9
        let str_amt: i32 = c.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Strength)
            .map(|p| p.amount)
            .unwrap();
        assert_eq!(str_amt, 3);
        let hp2 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp2 - (6 + 3));
    }

    /// 多怪物目标选择：打击只伤指定目标
    #[test]
    fn two_worms_targeting() {
        let mut c = Combat::new_with(9, 0, &[MonsterKind::JawWorm, MonsterKind::JawWorm]);
        let (hp0, hp1) = (c.monsters[0].creature.hp, c.monsters[1].creature.hp);
        force_hand(&mut c, &[CardInst::new(CardKind::Strike)]);
        let legal = c.legal_actions();
        assert_eq!(
            legal
                .iter()
                .filter(|a| matches!(a, Action::PlayCard { .. }))
                .count(),
            2,
            "两张目标各一个合法动作"
        );
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(1),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0);
        assert_eq!(c.monsters[1].creature.hp, hp1 - 6);
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
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
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
                while let Some(a) = c
                    .legal_actions()
                    .into_iter()
                    .find(|a| matches!(a, Action::PlayCard { .. }))
                {
                    c.step(&a);
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
        assert_ne!(t1, t3);
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

    /// 诚实型搜索地基：determinize 保持信息集不变（public_hash 相等），
    /// 但可能世界不同（state_hash 变化）；采样后的世界内部确定可复现
    #[test]
    fn determinize_preserves_information_set() {
        let mut a = Combat::new(21, 0);
        a.step(&Action::EndTurn); // 推进一回合让状态更"有意思"
        let pub0 = a.public_hash();
        let mut w1 = a.clone();
        let mut w2 = a.clone();
        w1.determinize(1001);
        w2.determinize(2002);
        assert_eq!(w1.public_hash(), pub0, "信息集不变");
        assert_eq!(w2.public_hash(), pub0);
        // 同一可能世界 + 同一动作序列 ⇒ 轨迹一致（确定性）
        let trace = |mut w: Combat| -> Vec<u64> {
            let mut t = vec![w.state_hash()];
            for _ in 0..4 {
                if w.over {
                    break;
                }
                let a = w
                    .legal_actions()
                    .into_iter()
                    .find(|x| matches!(x, Action::PlayCard { .. }))
                    .unwrap_or(Action::EndTurn);
                w.step(&a);
                t.push(w.state_hash());
            }
            t
        };
        let d1a = {
            let mut w = a.clone();
            w.determinize(1001);
            trace(w)
        };
        let d1b = {
            let mut w = a.clone();
            w.determinize(1001);
            trace(w)
        };
        assert_eq!(d1a, d1b, "同一采样世界必须确定");
    }
}
