//! M2 战斗状态机：数据驱动出牌（消费 generated 表的全部卡牌）。
//!
//! 执行序对照 docs/engine-semantics.md：
//! - 出牌：calculateCardDamage → use() 效果序列（Effect DSL 解释执行）→ UseCardAction 去向 → 扣能量
//! - 回合结束：玩家 powers atEndOfTurn（LoseStrength）→ 整手弃牌 → 怪物按列表序行动
//!   → applyEndOfTurnPowers（玩家衰减 → 逐怪衰减 + Ritual 投予）→ 下回合序列
//!
//! 诚实型搜索支撑：`Clone` / `determinize(world_seed)` / `public_hash()`
//! （docs/roadmap.md P4：信念 = 牌序排列 × 怪物未来 RNG 轨迹）

use std::hash::{Hash, Hasher};

use crate::cards::{engine_playable, ironclad_starter, CardInst};
use crate::damage::{calc_monster_damage, calc_player_card_block, calc_player_card_damage};
use crate::effects::{Effect, Expr, Target};
use crate::powers::{apply_power, DamageType, Decay, Power, PowerKind};
use crate::rng::{card_group_shuffle, RngStreams, StsRandom, Xs128};
use crate::state::{Creature, MonsterKind, MonsterState, PlayerState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// 打手牌第 index 张；指向性卡必须带目标怪物下标
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
    ApplyPowerToMonster {
        target: usize,
        kind: PowerKind,
        amount: i32,
    },
    ApplyPowerToSelf {
        kind: PowerKind,
        amount: i32,
    },
    Draw(i32),
    GainEnergy(i32),
    Heal(i32),
    LoseHP(i32),
    MakeDiscard(CardInst),
    RollMove(usize),
}

/// 效果 DSL 的 power id → PowerKind（ENGINE_POWERS 白名单）
fn power_kind(pid: &str) -> Option<PowerKind> {
    match pid {
        "Strength" => Some(PowerKind::Strength),
        "Vulnerable" => Some(PowerKind::Vulnerable),
        "Weakened" => Some(PowerKind::Weak),
        "Frail" => Some(PowerKind::Frail),
        "Ritual" => Some(PowerKind::Ritual),
        "LoseStrengthPower" => Some(PowerKind::LoseStrength),
        "Dexterity" => Some(PowerKind::Dexterity),
        "Poison" => Some(PowerKind::Poison),
        "Metallicize" => Some(PowerKind::Metallicize),
        "Regeneration" => Some(PowerKind::Regen),
        _ => None,
    }
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

    fn needs_target(card: &CardInst) -> bool {
        matches!(card.def.target, "ENEMY" | "SELF_AND_ENEMY")
    }

    fn can_play(&self, card: &CardInst) -> bool {
        if !engine_playable(card.def) {
            return false;
        }
        let cost = card.cost_pay();
        if cost == -1 {
            return true; // X 费：0 能量也可打（效果为 0 次）
        }
        cost >= 0 && cost <= self.player.energy
    }

    pub fn legal_actions(&self) -> Vec<Action> {
        let mut v = Vec::new();
        if self.over {
            return v;
        }
        for (i, card) in self.player.hand.iter().enumerate() {
            if !self.can_play(card) {
                continue;
            }
            if Self::needs_target(card) {
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

    fn eval(&self, e: Expr, card: CardInst, x_cost: i32) -> i32 {
        match e {
            Expr::N(n) => n,
            Expr::Magic => card.magic().unwrap_or(0),
            Expr::DamageBase => card.base_damage().unwrap_or(0),
            Expr::BlockBase => card.base_block().unwrap_or(0),
            Expr::CurrentBlock => self.player.creature.block,
            Expr::XCost => x_cost,
            Expr::Const(_) | Expr::Raw(_) => unreachable!("can_play 已过滤不可求值表达式"),
        }
    }

    fn play_card(&mut self, i: usize, target: Option<usize>) {
        let card = self.player.hand[i];
        let ti = target.unwrap_or(0);
        let x_cost = if card.cost_pay() == -1 {
            self.player.energy
        } else {
            0
        };
        let mut q: Vec<Q> = Vec::new();
        for &eff in card.def.effects {
            match eff {
                Effect::Damage(Target::Enemy, e) => {
                    let base = self.eval(e, card, x_cost);
                    let d = calc_player_card_damage(
                        base,
                        DamageType::Normal,
                        &self.player.creature.powers,
                        &self.monsters[ti].creature.powers,
                    );
                    q.push(Q::DamageMonster { target: ti, dmg: d });
                }
                // 可玩性过滤已排除 SelfC/Unknown 目标伤害
                Effect::Damage(_, _) => unreachable!("can_play 已过滤非敌方目标伤害"),
                Effect::DamageAll(e) => {
                    let base = self.eval(e, card, x_cost);
                    for (mi, mon) in self.monsters.iter().enumerate() {
                        if mon.creature.is_dead() {
                            continue;
                        }
                        // multiDamage：各目标分别过自己的 receive 管线
                        let d = calc_player_card_damage(
                            base,
                            DamageType::Normal,
                            &self.player.creature.powers,
                            &mon.creature.powers,
                        );
                        q.push(Q::DamageMonster { target: mi, dmg: d });
                    }
                }
                Effect::Block(e) => {
                    let base = self.eval(e, card, x_cost);
                    let b = calc_player_card_block(base, &self.player.creature.powers);
                    q.push(Q::PlayerBlock(b));
                }
                Effect::Draw(e) => q.push(Q::Draw(self.eval(e, card, x_cost))),
                Effect::ApplyPower(t, pid, e) => {
                    let n = self.eval(e, card, x_cost);
                    let kind = power_kind(pid).expect("can_play 已过滤 power 白名单");
                    match t {
                        Target::Enemy | Target::AllEnemies => {
                            q.push(Q::ApplyPowerToMonster {
                                target: ti,
                                kind,
                                amount: n,
                            });
                        }
                        _ => q.push(Q::ApplyPowerToSelf { kind, amount: n }),
                    }
                }
                Effect::GainEnergy(e) => q.push(Q::GainEnergy(self.eval(e, card, x_cost))),
                Effect::Heal(e) => q.push(Q::Heal(self.eval(e, card, x_cost))),
                Effect::LoseHP(e) => q.push(Q::LoseHP(self.eval(e, card, x_cost))),
                Effect::CopySelfToDiscard => q.push(Q::MakeDiscard(card)),
                Effect::MakeInDiscard(cls, e) => {
                    let n = self.eval(e, card, x_cost);
                    if let Some(c) = CardInst::new_by_class(cls) {
                        for _ in 0..n {
                            q.push(Q::MakeDiscard(c));
                        }
                    }
                }
                Effect::ExhaustHand | Effect::Unsupported(_) => {
                    unreachable!("can_play 已过滤不可玩效果")
                }
            }
        }
        // UseCardAction 去向：exhaust 旗标 → 消耗堆，否则弃牌堆
        self.player.hand.remove(i);
        if card.def.flags.contains(&"exhaust") {
            self.player.exhaust.push(card);
        } else {
            self.player.discard.push(card);
        }
        // 能量结算：X 费支付全部
        self.player.energy -= if card.cost_pay() == -1 {
            x_cost
        } else {
            card.cost_pay()
        };
        self.drain(q);
    }

    fn end_turn(&mut self) {
        self.turn_has_ended = true;
        // callEndOfTurnActions：玩家 powers 的 EOT，按插入序逐个结算：
        // Metallicize（PreEndTurnCards 钩子，先于其余）→ LoseStrength/Regen（atEndOfTurn）
        {
            let mut keep: Vec<Power> = Vec::new();
            let mut strength_deltas: Vec<i32> = Vec::new();
            for p in self.player.creature.powers.drain(..) {
                match p.kind {
                    PowerKind::Metallicize => {
                        self.player.creature.block += p.amount;
                        keep.push(p);
                    }
                    PowerKind::LoseStrength => strength_deltas.push(p.amount),
                    PowerKind::Regen => {
                        self.player.creature.hp =
                            (self.player.creature.hp + p.amount).min(self.player.creature.max_hp);
                        if p.amount > 1 {
                            keep.push(Power {
                                amount: p.amount - 1,
                                ..p
                            });
                        }
                    }
                    _ => keep.push(p),
                }
            }
            self.player.creature.powers = keep;
            for d in strength_deltas {
                apply_power(
                    &mut self.player.creature.powers,
                    PowerKind::Strength,
                    -d,
                    false,
                    false,
                );
            }
        }
        // DiscardAtEndOfTurn：无保留卡（retain）与 Runic Pyramid/Equilibrium 例外
        let hand = std::mem::take(&mut self.player.hand);
        self.player.discard.extend(hand);
        // 怪物序列（MonsterGroup.queueMonsters 列表序）
        for i in 0..self.monsters.len() {
            if !self.alive(i) {
                continue;
            }
            self.monsters[i].creature.block = 0; // pre-turn 掉格挡（调用点待 L1 核对）
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
        self.turn += 1;
        self.turn_has_ended = false;
        self.player.creature.block = 0;
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
                    q.push(Q::ApplyPowerToMonster {
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
                3 => q.push(Q::ApplyPowerToMonster {
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
    /// ① 怪 atEndOfTurn 触发（Poison tick：失 HP=层数后 -1，绕过格挡）
    /// ② 玩家 powers atEndOfRound（衰减）
    /// ③ 逐怪 powers atEndOfRound（衰减 + Ritual 投予）
    fn end_of_round_powers(&mut self) {
        for i in 0..self.monsters.len() {
            if !self.alive(i) {
                continue;
            }
            let mut poison_tick = 0;
            let mut keep: Vec<Power> = Vec::new();
            for p in self.monsters[i].creature.powers.drain(..) {
                if p.kind == PowerKind::Poison {
                    poison_tick += p.amount;
                    if p.amount > 1 {
                        keep.push(Power {
                            amount: p.amount - 1,
                            ..p
                        });
                    }
                } else {
                    keep.push(p);
                }
            }
            self.monsters[i].creature.powers = keep;
            if poison_tick > 0 {
                self.monsters[i].creature.hp -= poison_tick;
                if self.monsters[i].creature.is_dead() && self.all_monsters_dead() {
                    self.over = true;
                    self.won = true;
                    return;
                }
            }
        }
        decay_powers(&mut self.player.creature.powers);
        for i in 0..self.monsters.len() {
            if !self.alive(i) {
                continue;
            }
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
            Q::ApplyPowerToMonster {
                target,
                kind,
                amount,
            } => {
                if self.alive(target) {
                    apply_power(
                        &mut self.monsters[target].creature.powers,
                        kind,
                        amount,
                        false,
                        self.turn_has_ended,
                    );
                }
            }
            Q::ApplyPowerToSelf { kind, amount } => {
                apply_power(
                    &mut self.player.creature.powers,
                    kind,
                    amount,
                    false,
                    self.turn_has_ended,
                );
            }
            Q::Draw(n) => self.draw_cards(n.max(0) as usize),
            Q::GainEnergy(n) => self.player.energy += n,
            Q::Heal(n) => {
                self.player.creature.hp =
                    (self.player.creature.hp + n).min(self.player.creature.max_hp);
            }
            Q::LoseHP(n) => {
                self.player.creature.hp -= n; // HP 损失绕过格挡
                if self.player.creature.is_dead() {
                    self.over = true;
                    self.won = false;
                }
            }
            Q::MakeDiscard(c) => self.player.discard.push(c),
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
                self.player.discard.push(c);
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
            return 1;
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

    /// 规则状态稳定哈希（含隐藏信息，供对拍/复现测试）
    pub fn state_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_public(&mut h);
        for c in &self.player.draw {
            (c.def.id, c.upgraded).hash(&mut h);
        }
        self.rng.ai.counter.hash(&mut h);
        self.rng.shuffle.counter.hash(&mut h);
        self.rng.monster_hp.counter.hash(&mut h);
        h.finish()
    }

    /// 信息集哈希（公开等价类）：抽牌堆按多重集、不含 RNG 计数器
    pub fn public_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash_public(&mut h);
        let mut draw_set: Vec<(&str, bool, usize)> = self
            .player
            .draw
            .iter()
            .map(|c| (c.def.id, c.upgraded, 0usize))
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
            (c.def.id, c.upgraded).hash(h);
        }
        for c in &self.player.discard {
            (c.def.id, c.upgraded).hash(h);
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

    /// 诚实型信念采样：抽牌堆未知位置均匀排列 + ai/shuffle 流重播种。
    ///
    /// 采样后局面内部完全确定，可做确定性推演。
    pub fn determinize(&mut self, world_seed: u64) {
        let mut r = Xs128::new(world_seed);
        let n = self.player.draw.len();
        for i in (1..n).rev() {
            let j = r.next_long_n(i as u64 + 1) as usize;
            self.player.draw.swap(i, j);
        }
        self.rng.ai = StsRandom::new(r.next_long());
        self.rng.shuffle = StsRandom::new(r.next_long());
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
    use crate::generated::CARDS;

    fn force_hand(c: &mut Combat, ids: &[&str]) {
        let old = std::mem::take(&mut c.player.hand);
        c.player.draw.extend(old);
        c.player.hand = ids.iter().map(|id| CardInst::new(id)).collect();
    }

    /// 数据驱动覆盖：引擎当前可完整执行的卡牌数
    #[test]
    fn engine_playable_count() {
        let n = CARDS.iter().filter(|d| engine_playable(d)).count();
        println!("engine_playable = {n}/{}", CARDS.len());
        // 上限 = 375 - 系统性未支持(约160) - 本就不可打出(-2费约25)；
        // 余量主要被未实现 power 持有（每个 power 解锁若干卡，见 docs/coverage.md）
        assert!(n >= 88, "数据驱动出牌应达到当前硬上限附近，当前 {n}");
    }

    #[test]
    fn bash_vulnerable_then_strike() {
        let mut c = Combat::new(42, 0);
        let hp0 = c.monsters[0].creature.hp;
        force_hand(&mut c, &["Bash"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 8);
        assert_eq!(c.monsters[0].creature.powers[0].amount, 2);
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, 80 - 11);
        assert_eq!(c.monsters[0].creature.powers[0].amount, 1);
        force_hand(&mut c, &["Strike_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 8 - 9); // floor(6×1.5)
        c.step(&Action::EndTurn);
        assert!(c.monsters[0].creature.powers.is_empty());
    }

    /// 双击：两张打击的伤害（Twin Strike 2×5? 打击G=5？铁甲 Twin Strike 基伤 5×2）
    #[test]
    fn twin_strike_hits_twice() {
        let mut c = Combat::new(4, 0);
        let hp0 = c.monsters[0].creature.hp;
        force_hand(&mut c, &["Twin Strike"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        let dmg = c.monsters[0].creature.max_hp; // 占位防警告
        let _ = dmg;
        let dealt = hp0 - c.monsters[0].creature.hp;
        assert_eq!(dealt, 10, "Twin Strike 5×2=10（源码 baseDamage=5）");
    }

    /// 顺劈：双颚虫各吃 8
    #[test]
    fn cleave_hits_all() {
        let mut c = Combat::new_with(9, 0, &[MonsterKind::JawWorm, MonsterKind::JawWorm]);
        let (hp0, hp1) = (c.monsters[0].creature.hp, c.monsters[1].creature.hp);
        force_hand(&mut c, &["Cleave"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        assert_eq!(hp0 - c.monsters[0].creature.hp, 8);
        assert_eq!(hp1 - c.monsters[1].creature.hp, 8);
    }

    /// 锁喉：伤害 + 虚弱 2 层
    #[test]
    fn clothesline_applies_weak() {
        let mut c = Combat::new(6, 0);
        force_hand(&mut c, &["Clothesline"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        let weak = c.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Weak)
            .unwrap();
        assert_eq!(weak.amount, 2);
    }

    /// 屈伸：+2 力量，回合末失效
    #[test]
    fn flex_temporary_strength() {
        let mut c = Combat::new(3, 0);
        force_hand(&mut c, &["Flex"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        let str_amt: i32 = c
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Strength)
            .map(|p| p.amount)
            .unwrap();
        assert_eq!(str_amt, 2);
        c.step(&Action::EndTurn);
        assert!(
            !c.player
                .creature
                .powers
                .iter()
                .any(|p| p.kind == PowerKind::Strength),
            "回合末屈伸力量应失效"
        );
    }

    /// 身痛猛击：伤害 = 当前格挡（先防御 5 再猛击 5）
    #[test]
    fn body_slam_uses_block() {
        let mut c = Combat::new(5, 0);
        let hp0 = c.monsters[0].creature.hp;
        force_hand(&mut c, &["Defend_R", "Body Slam"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        }); // 防御 5
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        }); // Body Slam（原 index 1 前移）
        assert_eq!(hp0 - c.monsters[0].creature.hp, 5);
    }

    /// 怒气：打出后弃牌堆多一张复制
    #[test]
    fn anger_copies_to_discard() {
        let mut c = Combat::new(8, 0);
        force_hand(&mut c, &["Anger"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(
            c.player
                .discard
                .iter()
                .filter(|x| x.def.id == "Anger")
                .count(),
            2,
            "打出的本体 + 生成的复制"
        );
    }

    /// 掌击：伤害 + 抽 1
    #[test]
    fn pommel_strike_draws() {
        let mut c = Combat::new(2, 0);
        force_hand(&mut c, &["Pommel Strike"]);
        let before = c.player.hand.len();
        let draw_before = c.player.draw.len();
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.player.hand.len(), before, "打出1张+抽1张");
        assert_eq!(c.player.draw.len(), draw_before - 1);
    }

    #[test]
    fn cultist_ritual_growth() {
        let mut c = Combat::new_with(11, 0, &[MonsterKind::Cultist]);
        c.step(&Action::EndTurn);
        assert!(!c.monsters[0]
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Strength));
        let hp1 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp1 - 6);
        let hp2 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp2 - (6 + 3));
    }

    /// 战斗可完整打完，且同种子同策略轨迹逐点一致
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
        assert!(t1.len() < 200);
        let (t3, _, _) = run(8);
        assert_ne!(t1, t3);
    }

    #[test]
    fn determinize_preserves_information_set() {
        let mut a = Combat::new(21, 0);
        a.step(&Action::EndTurn);
        let pub0 = a.public_hash();
        let mut w1 = a.clone();
        w1.determinize(1001);
        let mut w2 = a.clone();
        w2.determinize(2002);
        assert_eq!(w1.public_hash(), pub0);
        assert_eq!(w2.public_hash(), pub0);
    }
}
