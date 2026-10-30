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
    /// 响应手牌选择屏（弃牌类）：要弃的手牌下标集（选择序）
    HandSelect {
        indices: Vec<usize>,
    },
    /// 响应占卜：shown 中要弃掉的下标子集，其余保持原序置顶
    HandPickResponse {
        index: usize,
    },
    PilePickResponse {
        index: usize,
    },
    ScryChoice {
        discard_indices: Vec<usize>,
    },
}

/// 战斗内挂起的选择屏（协议 v0.2 选择屏三件套之二）
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HandPickPurpose {
    PutOnDeckTop,
    PutOnDeckBottom,
    UpgradeCard,
    FreeCost,
    ExhaustGainEnergy,
    DuplicateAttack,
    CopyNextTurn,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PilePickPurpose {
    ExhaustToHand,
    DiscardToDeckTop,
    DiscardToHand,
    DrawPileToHand,
    DrawPlayTwiceExhaust,
    DiscardExhaustPick,
}

#[derive(Clone, Debug)]
pub enum Pending {
    /// 选恰好 amount 张弃掉（DiscardAction，手牌 > amount 时才有屏）
    HandDiscard {
        amount: usize,
    },
    /// 弃任意张（amount<0 → open(99, anyNumber, canPickZero)）
    HandDiscardAny,
    /// 占卜：展示的牌（顺序：抽牌堆顶 → 底）
    Scry {
        shown: Vec<CardInst>,
    },
    HandPick {
        purpose: HandPickPurpose,
    },
    PilePick {
        pile: &'static str,
        purpose: PilePickPurpose,
    },
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
    /// 随机敌人伤害：结算时以 cardRandomRng 选存活目标（getRandomMonster 复刻）
    DamageRandom {
        base: i32,
    },
    MakeInHand(CardInst),
    MakeInDraw {
        inst: CardInst,
        random: bool,
    },
    RequireHandDiscard {
        amount: i32,
    },
    RequireScry {
        n: i32,
    },
    RandomDiscard {
        n: i32,
    },
    ChangeStance(crate::state::Stance),
    Channel {
        kind: crate::state::OrbKind,
        auto_evoke: bool,
    },
    EvokeLeftmost,
    EvokeNoRemove,
    OrbSlots(i32),
    DoDrawToHand(i32),
    DoSetHandCostOne,
    DoModifySelfDamage(i32),
    DoSelectUpgradeCard,
    DoEvokeAllOrbs,
    DoSelectAttackDuplicate,
    DoSelectCopyNextTurn(i32),
    DoIfKillSelectUpgrade {
        target: usize,
    },
    DoSelectDrawPlayTwiceExhaust,
    DoSelectDiscardExhaust,
    DoTriggerAllOrbPassives,
    DoTriggerDarkOrbPassives,
    DoIfKillGainGold {
        target: usize,
        gold_gain: i32,
    },
    DoSelectExhaustGainEnergyCost,
    DoReturnZeroCostFromDiscard,
    DoUpgradeAllHand,
    DoUpgradeStrikesDefends,
    DoBlockPerDiscard(i32),
    DoPlayTopCard,
    DoSelectFreeCost,
    DoModifySelfBlock(i32),
    DoModifySelfCost(i32),
    DoIfKillModifySelfDamage {
        target: usize,
        damage_gain: i32,
    },
    DoNextTurnDraw(i32),
    DoNextTurnGainEnergy(i32),
    DoIfNoAttacksDraw(i32),
    DoIfKillGainEnergy {
        target: usize,
        energy_gain: i32,
    },
    DoMoveTypeToHand(&'static str),
    DoRemoveAllBlock {
        target: usize,
    },
    DoSelectDrawPileToHand,
    DoIfLastCardSkill(i32),
    DoIfIntentApplyWeak {
        target: usize,
        weak_amt: i32,
    },
    DoSelectDiscardToHand,
    DoSelectPutOnDeckTop,
    DoSelectPutOnDeckBottom,
    DoSelectExhaustToHand,
    DoSelectDiscardToTop,
    DoIfDiscardedGainEnergy(i32),
    DoIfStanceBlock {
        stance: crate::state::Stance,
        block: i32,
    },
    ShuffleDraw,
    GainBlock(i32),
    DoDiscardAllHand,
    DoSkipEnemyTurn,
    DoVampireBlock {
        target: usize,
        base: i32,
    },
    DoExecuteEnemy {
        target: usize,
        cutoff: i32,
    },
    DoGainEnergyDrawPile {
        divide: i32,
    },
    DoSetHandCostZero,
    DoExhaustHand,
    VampireMaxHp {
        target: usize,
        base: i32,
        max_hp_gain: i32,
    },
    IfIntentAttack {
        target: usize,
        str_gain: i32,
    },
    DoDiscardNonAttacks,
    ForceEndTurn,
    VampireDamageAll {
        base: i32,
    },
    ModifyPowerMonster {
        target: usize,
        kind: PowerKind,
        mult: i32,
    },
    ModifyPowerSelf {
        kind: PowerKind,
        mult: i32,
    },
    DamageIfPoisoned {
        target: usize,
        base: i32,
    },
    ExhaustNonAttacks {
        block_per: i32,
    },
    DamagePerAttack {
        target: usize,
        base: i32,
    },
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
        "Focus" => Some(PowerKind::Focus),
        "Demon Form" => Some(PowerKind::DemonForm),
        "Berserk" => Some(PowerKind::Berserk),
        "Brutality" => Some(PowerKind::Brutality),
        "Barricade" => Some(PowerKind::Barricade),
        "Thorns" => Some(PowerKind::Thorns),
        "Artifact" => Some(PowerKind::Artifact),
        "IntangiblePlayer" => Some(PowerKind::IntangiblePlayer),
        "Energized" => Some(PowerKind::Energized),
        "Combust" => Some(PowerKind::Combust),
        "Dark Embrace" => Some(PowerKind::DarkEmbrace),
        "Rage" => Some(PowerKind::Rage),
        "Curl Up" => Some(PowerKind::CurlUp),
        "Spore Cloud" => Some(PowerKind::SporeCloud),
        _ => None,
    }
}

#[derive(Clone)]
pub struct Combat {
    pub player: PlayerState,
    pub monsters: Vec<MonsterState>,
    pub rng: RngStreams,
    /// 挂起的选择屏（None = 正常行动阶段）
    pub pending: Option<Pending>,
    /// 选择屏挂起时暂存的动作队列
    pending_queue: Option<Vec<Q>>,
    pub attacks_this_turn: i32,
    hand_cost_zero_this_turn: bool,
    hand_cost_max_one: bool,
    pub next_turn_draw: i32,
    pub next_turn_energy: i32,
    pub gold: i32,
    pub next_turn_copies: Vec<(CardInst, i32)>,
    pub next_turn_copy_count: i32,
    skip_monster_turn: bool,
    force_end_turn_after_queue: bool,
    pub discarded_this_turn: i32,
    pub last_card_type: Option<String>,
    pub turn: i32,
    pub turn_has_ended: bool,
    pub over: bool,
    pub won: bool,
    pub ascension: i32,
}

/// 项目默认状态：全解锁 + 进阶 20（评测基准，见 README/roadmap）
pub const DEFAULT_ASCENSION: i32 = 20;

impl Combat {
    /// 默认遭遇：单大颚虫，默认进阶（A20，全解锁）
    pub fn new(seed: u64) -> Self {
        Self::new_asc(seed, DEFAULT_ASCENSION)
    }

    /// 指定进阶等级的默认遭遇
    pub fn new_asc(seed: u64, ascension: i32) -> Self {
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
                stance: crate::state::Stance::Neutral,
                orbs: Vec::new(),
                max_orbs: 3, // Defect 默认3槽（M2 简化：仅 Defect 有充能球）
                // A6+：起始 90% HP（AbstractDungeon:2574）；TODO(M3)：药水栏/金币/休息治疗等局外修正
                creature: {
                    let max_hp = 80;
                    let cur = if ascension >= 6 {
                        (max_hp as f32 * 0.9).round() as i32
                    } else {
                        max_hp
                    };
                    Creature::new(cur, max_hp)
                },
                energy: 3,
                hand: Vec::new(),
                draw: ironclad_starter(ascension),
                discard: Vec::new(),
                exhaust: Vec::new(),
            },
            monsters,
            rng,
            pending: None,
            pending_queue: None,
            attacks_this_turn: 0,
            hand_cost_zero_this_turn: false,
            hand_cost_max_one: false,
            next_turn_draw: 0,
            next_turn_energy: 0,
            gold: 99,
            next_turn_copies: Vec::new(),
            next_turn_copy_count: 0,
            skip_monster_turn: false,
            force_end_turn_after_queue: false,
            discarded_this_turn: 0,
            last_card_type: None,
            turn: 1,
            turn_has_ended: false,
            over: false,
            won: false,
            ascension,
        };
        card_group_shuffle(&mut c.player.draw, &mut c.rng.shuffle);
        c.draw_cards(5);
        for i in 0..c.monsters.len() {
            match c.monsters[i].kind {
                MonsterKind::LouseNormal => {
                    let curl = if ascension >= 17 {
                        c.rng.monster_hp.random_start_end(9, 12)
                    } else if ascension >= 7 {
                        c.rng.monster_hp.random_start_end(4, 8)
                    } else {
                        c.rng.monster_hp.random_start_end(3, 7)
                    };
                    apply_power(
                        &mut c.monsters[i].creature.powers,
                        PowerKind::CurlUp,
                        curl,
                        false,
                        false,
                    );
                }
                MonsterKind::FungiBeast => {
                    apply_power(
                        &mut c.monsters[i].creature.powers,
                        PowerKind::SporeCloud,
                        2,
                        false,
                        false,
                    );
                }
                _ => {}
            }
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
        (0..=self.player.energy).contains(&cost)
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
                self.player.stance,
            )
        })
    }

    pub fn step(&mut self, a: &Action) {
        if self.over {
            return;
        }
        match a {
            Action::PlayCard { index, target } => {
                assert!(self.pending.is_none(), "选择屏挂起中不可出牌");
                self.play_card(*index, *target);
            }
            Action::EndTurn => {
                assert!(self.pending.is_none(), "选择屏挂起中不可结束回合");
                self.end_turn();
            }
            Action::HandSelect { indices } => self.resolve_hand_select(indices),
            Action::ScryChoice { discard_indices } => self.resolve_scry(discard_indices),
            Action::HandPickResponse { index } => self.resolve_hand_pick(*index),
            Action::PilePickResponse { index } => self.resolve_pile_pick(*index),
        }
    }

    /// 弃牌选择响应：选中卡按选择序入弃牌堆，随后恢复暂存队列
    fn resolve_hand_select(&mut self, indices: &[usize]) {
        match self.pending.clone() {
            Some(Pending::HandDiscard { amount }) => {
                assert_eq!(indices.len(), amount, "必须恰好选择 {amount} 张");
                let mut sel = indices.to_vec();
                sel.sort_unstable();
                sel.dedup();
                assert_eq!(sel.len(), amount, "下标不得重复");
                for &i in sel.iter().rev() {
                    let c = self.player.hand.remove(i);
                    self.player.discard.push(c);
                }
            }
            Some(Pending::HandDiscardAny) => {
                let mut sel = indices.to_vec();
                sel.sort_unstable();
                sel.dedup();
                for &i in sel.iter().rev() {
                    let c = self.player.hand.remove(i);
                    self.player.discard.push(c);
                }
            }
            _ => panic!("当前无弃牌选择屏"),
        }
        self.resume_queue();
    }

    /// 占卜响应：弃掉所选，其余保持原序置顶（draw 尾 = 顶）
    fn resolve_scry(&mut self, discard_indices: &[usize]) {
        let shown = match self.pending.clone() {
            Some(Pending::Scry { shown }) => shown,
            _ => panic!("当前无占卜屏"),
        };
        let n = shown.len();
        let mut chosen: Vec<usize> = discard_indices.to_vec();
        chosen.sort_unstable();
        chosen.dedup();
        assert!(chosen.iter().all(|&i| i < n), "下标越界");
        // shown[i] 对应 draw[len-n+i]；倒序移除避免位移
        let len = self.player.draw.len();
        for &i in chosen.iter().rev() {
            let c = self.player.draw.remove(len - n + i);
            self.player.discard.push(c);
        }
        self.resume_queue();
    }

    fn resolve_hand_pick(&mut self, index: usize) {
        if let Some(Pending::HandPick { purpose }) = self.pending.clone() {
            if index < self.player.hand.len() {
                let c = self.player.hand.remove(index);
                match purpose {
                    HandPickPurpose::PutOnDeckTop => self.player.draw.push(c),
                    HandPickPurpose::PutOnDeckBottom => self.player.draw.insert(0, c),
                    HandPickPurpose::UpgradeCard => {
                        let mut c = c;
                        c.upgraded = true;
                        self.player.hand.push(c);
                    }
                    HandPickPurpose::FreeCost => {
                        let mut c = c;
                        c.cost_delta = -(c.def.cost.unwrap_or(0)).max(0);
                        self.player.hand.push(c);
                    }
                    HandPickPurpose::ExhaustGainEnergy => {
                        let energy = c.cost_pay().max(0);
                        self.player.energy += energy;
                        self.player.exhaust.push(c);
                    }
                    HandPickPurpose::CopyNextTurn => {
                        // Night Terror：记录卡+数量，下回合加副本
                        let n = self.next_turn_copy_count.max(0);
                        self.next_turn_copies.push((c, n));
                        self.player.hand.push(c); // 原卡回手
                    }
                    HandPickPurpose::DuplicateAttack => {
                        // 原卡回手 + 一份副本（Dual Wield 基础版）
                        self.player.hand.push(c);
                        if self.player.hand.len() < 10 {
                            self.player.hand.push(c);
                        }
                    }
                }
            }
        }
        self.resume_queue();
    }

    fn resolve_pile_pick(&mut self, index: usize) {
        if let Some(Pending::PilePick { pile, .. }) = self.pending.clone() {
            match pile {
                "draw_play" => {
                    // Omniscience：选卡执行两次并消耗
                    if index < self.player.draw.len() {
                        let card = self.player.draw.remove(index);
                        for _play in 0..2 {
                            let mut q2 = Vec::new();
                            for &eff in card.def.effects {
                                self.apply_effect(eff, card, 0, 0, &mut q2, 0);
                            }
                            self.drain(q2);
                        }
                        self.player.exhaust.push(card);
                    }
                }
                "discard_exhaust" => {
                    // Meditate：从弃牌堆选卡消耗
                    if index < self.player.discard.len() {
                        let c = self.player.discard.remove(index);
                        // 如果是技能获得1能量（近似）
                        if c.def.card_type == "SKILL" {
                            self.player.energy += 1;
                        }
                        self.player.exhaust.push(c);
                    }
                }
                "exhaust" => {
                    if index < self.player.exhaust.len() {
                        let c = self.player.exhaust.remove(index);
                        if self.player.hand.len() < 10 {
                            self.player.hand.push(c);
                        }
                    }
                }
                "discard" => {
                    if index < self.player.discard.len() {
                        let c = self.player.discard.remove(index);
                        self.player.draw.push(c);
                    }
                }
                _ => {}
            }
        }
        self.resume_queue();
    }

    fn resume_queue(&mut self) {
        self.pending = None;
        if let Some(q) = self.pending_queue.take() {
            self.drain(q);
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
            Expr::HandSize => self.player.hand.len() as i32,
            Expr::DrawPileSize => self.player.draw.len() as i32,
            Expr::CurrentEnergy => self.player.energy,
            Expr::OrbCount => self.player.orbs.len() as i32,
            Expr::DiscardPileSize => self.player.discard.len() as i32,
            Expr::UniqueOrbTypes => {
                let mut types = std::collections::HashSet::new();
                for orb in &self.player.orbs {
                    types.insert(orb.kind);
                }
                types.len() as i32
            }
            Expr::NegXCost => -x_cost,
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
        // onUseCard 钩子（先于卡牌效果）：Rage —— 打出攻击牌获得 amount 格挡
        self.last_card_type = Some(card.def.card_type.to_string());
        if card.def.card_type == "ATTACK" {
            if let Some(rage) = self
                .player
                .creature
                .powers
                .iter()
                .find(|pw| pw.kind == PowerKind::Rage)
            {
                q.push(Q::PlayerBlock(rage.amount));
            }
        }
        for &eff in card.def.effects {
            self.apply_effect(eff, card, ti, x_cost, &mut q, 0);
        }
        // UseCardAction 去向：exhaust 旗标 → 消耗堆，否则弃牌堆
        self.player.hand.remove(i);
        if card.def.flags.contains(&"exhaust") {
            self.player.exhaust.push(card);
            // onExhaust 钩子：Dark Embrace —— 每消耗一张抽 amount
            if let Some(de) = self
                .player
                .creature
                .powers
                .iter()
                .find(|pw| pw.kind == PowerKind::DarkEmbrace)
            {
                let n = de.amount;
                self.draw_cards(n as usize);
            }
        } else {
            self.player.discard.push(card);
        }
        // 追踪本回合攻击牌数（Finisher 用）
        self.last_card_type = Some(card.def.card_type.to_string());
        if card.def.card_type == "ATTACK" {
            self.attacks_this_turn += 1;
        }
        // 能量结算：X 费支付全部
        self.player.energy -= if card.cost_pay() == -1 {
            x_cost
        } else {
            card.cost_pay()
        };
        self.drain(q);
    }

    /// 效果 DSL 递归解释执行（Repeat 展开，深度防御上限 8）
    fn apply_effect(
        &self,
        eff: Effect,
        card: CardInst,
        ti: usize,
        x_cost: i32,
        q: &mut Vec<Q>,
        depth: u32,
    ) {
        if depth > 8 {
            return;
        }
        match eff {
            Effect::Damage(Target::Enemy, e) => {
                let base = self.eval(e, card, x_cost);
                let d = calc_player_card_damage(
                    base,
                    DamageType::Normal,
                    &self.player.creature.powers,
                    &self.monsters[ti].creature.powers,
                    self.player.stance,
                );
                q.push(Q::DamageMonster { target: ti, dmg: d });
            }
            Effect::DamageAll(e) => {
                let base = self.eval(e, card, x_cost);
                for (mi, mon) in self.monsters.iter().enumerate() {
                    if mon.creature.is_dead() {
                        continue;
                    }
                    let d = calc_player_card_damage(
                        base,
                        DamageType::Normal,
                        &self.player.creature.powers,
                        &mon.creature.powers,
                        self.player.stance,
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
            Effect::Repeat(n_expr, inner) => {
                let n = self.eval(n_expr, card, x_cost).clamp(0, 999);
                for _ in 0..n {
                    for &ie in inner {
                        self.apply_effect(ie, card, ti, x_cost, q, depth + 1);
                    }
                }
            }
            Effect::DamageRandom(e) => {
                let base = self.eval(e, card, x_cost);
                q.push(Q::DamageRandom { base });
            }
            Effect::MakeInHand(cls, e) => {
                let n = self.eval(e, card, x_cost);
                if let Some(c) = CardInst::new_by_class(cls) {
                    for _ in 0..n {
                        q.push(Q::MakeInHand(c));
                    }
                }
            }
            Effect::CopySelfToHand(e) => {
                let n = self.eval(e, card, x_cost);
                for _ in 0..n {
                    q.push(Q::MakeInHand(card));
                }
            }
            Effect::MakeInDrawRandom(cls, e) | Effect::MakeInDrawTop(cls, e) => {
                let n = self.eval(e, card, x_cost);
                let random = matches!(eff, Effect::MakeInDrawRandom(_, _));
                if let Some(c) = CardInst::new_by_class(cls) {
                    for _ in 0..n {
                        q.push(Q::MakeInDraw { inst: c, random });
                    }
                }
            }
            Effect::HandDiscard(e, is_random) => {
                let n = self.eval(e, card, x_cost);
                if is_random {
                    // isRandom=true：无选择屏，exec 时以 getRandomCard(cardRandomRng) 自动弃
                    q.push(Q::RandomDiscard { n });
                } else {
                    q.push(Q::RequireHandDiscard { amount: n });
                }
            }
            Effect::Scry(e) => {
                q.push(Q::RequireScry {
                    n: self.eval(e, card, x_cost),
                });
            }
            Effect::ChangeStance(sid) => {
                if let Some(st) = crate::state::Stance::from_id(sid) {
                    q.push(Q::ChangeStance(st));
                }
            }
            Effect::Channel(orb_id, e, auto) => {
                if let Some(kind) = crate::state::OrbKind::from_id(orb_id) {
                    let n = self.eval(e, card, x_cost);
                    for _ in 0..n {
                        q.push(Q::Channel {
                            kind,
                            auto_evoke: auto,
                        });
                    }
                }
            }
            Effect::EvokeLeftmost => q.push(Q::EvokeLeftmost),
            Effect::EvokeNoRemove => q.push(Q::EvokeNoRemove),
            Effect::OrbSlots(e) => q.push(Q::OrbSlots(self.eval(e, card, x_cost))),
            Effect::ShuffleDraw => q.push(Q::ShuffleDraw),
            Effect::IfStanceBlock(sid, e) => {
                if let Some(st) = crate::state::Stance::from_id(sid) {
                    q.push(Q::DoIfStanceBlock {
                        stance: st,
                        block: self.eval(e, card, x_cost),
                    });
                }
            }
            Effect::SkipEnemyTurn => q.push(Q::DoSkipEnemyTurn),
            Effect::VampireBlock(e) => q.push(Q::DoVampireBlock {
                target: ti,
                base: self.eval(e, card, x_cost),
            }),
            Effect::ExecuteEnemy(e) => q.push(Q::DoExecuteEnemy {
                target: ti,
                cutoff: self.eval(e, card, x_cost),
            }),
            Effect::GainEnergyDrawPile(e) => q.push(Q::DoGainEnergyDrawPile {
                divide: self.eval(e, card, x_cost).max(1),
            }),
            Effect::DiscardAllHand => q.push(Q::DoDiscardAllHand),
            Effect::SetHandCostZero => q.push(Q::DoSetHandCostZero),
            Effect::VampireMaxHp(e) => {
                let gain = self.eval(e, card, x_cost);
                q.push(Q::VampireMaxHp {
                    target: ti,
                    base: card.base_damage().unwrap_or(0),
                    max_hp_gain: gain,
                });
            }
            Effect::ExhaustHand => q.push(Q::DoExhaustHand),
            Effect::IfIntentAttack(e) => q.push(Q::IfIntentAttack {
                target: ti,
                str_gain: self.eval(e, card, x_cost),
            }),
            Effect::DiscardNonAttacks => q.push(Q::DoDiscardNonAttacks),
            Effect::GainBlock(e) => q.push(Q::GainBlock(self.eval(e, card, x_cost))),
            Effect::ForceEndTurn => q.push(Q::ForceEndTurn),
            Effect::VampireDamageAll(e) => q.push(Q::VampireDamageAll {
                base: self.eval(e, card, x_cost),
            }),
            Effect::ModifyPower(t, pid, mult) => {
                if let Some(kind) = power_kind(pid) {
                    match t {
                        Target::Enemy | Target::AllEnemies => {
                            q.push(Q::ModifyPowerMonster {
                                target: ti,
                                kind,
                                mult,
                            });
                        }
                        _ => q.push(Q::ModifyPowerSelf { kind, mult }),
                    }
                }
            }
            Effect::DamageIfPoisoned(e) => q.push(Q::DamageIfPoisoned {
                target: ti,
                base: self.eval(e, card, x_cost),
            }),
            Effect::ExhaustNonAttacks(e) => q.push(Q::ExhaustNonAttacks {
                block_per: self.eval(e, card, x_cost),
            }),
            Effect::DamagePerAttack(e) => q.push(Q::DamagePerAttack {
                target: ti,
                base: self.eval(e, card, x_cost),
            }),
            Effect::SelectCopyNextTurn(e) => {
                q.push(Q::DoSelectCopyNextTurn(self.eval(e, card, x_cost)));
            }
            Effect::IfKillSelectUpgrade => q.push(Q::DoIfKillSelectUpgrade { target: ti }),
            Effect::SelectAttackDuplicate => q.push(Q::DoSelectAttackDuplicate),
            Effect::SelectDrawPlayTwiceExhaust => q.push(Q::DoSelectDrawPlayTwiceExhaust),
            Effect::SelectDiscardExhaust => q.push(Q::DoSelectDiscardExhaust),
            Effect::EvokeAllOrbs => q.push(Q::DoEvokeAllOrbs),
            Effect::TriggerAllOrbPassives => q.push(Q::DoTriggerAllOrbPassives),
            Effect::TriggerDarkOrbPassives => q.push(Q::DoTriggerDarkOrbPassives),
            Effect::IfKillGainGold(e) => q.push(Q::DoIfKillGainGold {
                target: ti,
                gold_gain: self.eval(e, card, x_cost),
            }),
            Effect::SelectExhaustGainEnergyCost => q.push(Q::DoSelectExhaustGainEnergyCost),
            Effect::ReturnZeroCostFromDiscard => q.push(Q::DoReturnZeroCostFromDiscard),
            Effect::SelectUpgradeCard => q.push(Q::DoSelectUpgradeCard),
            Effect::UpgradeAllHand => q.push(Q::DoUpgradeAllHand),
            Effect::UpgradeStrikesDefends => q.push(Q::DoUpgradeStrikesDefends),
            Effect::BlockPerDiscard(e) => q.push(Q::DoBlockPerDiscard(self.eval(e, card, x_cost))),
            Effect::PlayTopCard => q.push(Q::DoPlayTopCard),
            Effect::SelectFreeCost => q.push(Q::DoSelectFreeCost),
            Effect::ModifySelfDamage(e) => {
                q.push(Q::DoModifySelfDamage(self.eval(e, card, x_cost)));
            }
            Effect::ModifySelfBlock(e) => {
                q.push(Q::DoModifySelfBlock(self.eval(e, card, x_cost)));
            }
            Effect::ModifySelfCost(e) => {
                q.push(Q::DoModifySelfCost(self.eval(e, card, x_cost)));
            }
            Effect::IfKillModifySelfDamage(e) => {
                q.push(Q::DoIfKillModifySelfDamage {
                    target: ti,
                    damage_gain: self.eval(e, card, x_cost),
                });
            }
            Effect::NextTurnDraw(e) => {
                q.push(Q::DoNextTurnDraw(self.eval(e, card, x_cost)));
            }
            Effect::NextTurnGainEnergy(e) => {
                q.push(Q::DoNextTurnGainEnergy(self.eval(e, card, x_cost)));
            }
            Effect::SetHandCostOne => q.push(Q::DoSetHandCostOne),
            Effect::IfNoAttacksDraw(e) => q.push(Q::DoIfNoAttacksDraw(self.eval(e, card, x_cost))),
            Effect::IfKillGainEnergy(e) => q.push(Q::DoIfKillGainEnergy {
                target: ti,
                energy_gain: self.eval(e, card, x_cost),
            }),
            Effect::MoveTypeToHand(t) => q.push(Q::DoMoveTypeToHand(t)),
            Effect::RemoveAllBlock => q.push(Q::DoRemoveAllBlock { target: ti }),
            Effect::IfStance(sid, inner) => {
                if let Some(st) = crate::state::Stance::from_id(sid) {
                    if self.player.stance == st {
                        for &ie in inner {
                            self.apply_effect(ie, card, ti, x_cost, q, depth + 1);
                        }
                    }
                }
            }
            Effect::IfNotStance(sid, inner) => {
                if let Some(st) = crate::state::Stance::from_id(sid) {
                    if self.player.stance != st {
                        for &ie in inner {
                            self.apply_effect(ie, card, ti, x_cost, q, depth + 1);
                        }
                    }
                }
            }
            Effect::SelectDrawPileToHand => q.push(Q::DoSelectDrawPileToHand),
            Effect::IfLastCardSkill(e) => {
                q.push(Q::DoIfLastCardSkill(self.eval(e, card, x_cost)));
            }
            Effect::DrawToHand(e) => q.push(Q::DoDrawToHand(self.eval(e, card, x_cost))),
            Effect::IfIntentApplyWeak(e) => q.push(Q::DoIfIntentApplyWeak {
                target: ti,
                weak_amt: self.eval(e, card, x_cost),
            }),
            Effect::SelectDiscardToHand => q.push(Q::DoSelectDiscardToHand),
            Effect::IfDiscardedGainEnergy(e) => {
                q.push(Q::DoIfDiscardedGainEnergy(self.eval(e, card, x_cost)));
            }
            Effect::SelectPutOnDeckTop => q.push(Q::DoSelectPutOnDeckTop),
            Effect::SelectPutOnDeckBottom => q.push(Q::DoSelectPutOnDeckBottom),
            Effect::SelectExhaustToHand => q.push(Q::DoSelectExhaustToHand),
            Effect::SelectDiscardToTop => q.push(Q::DoSelectDiscardToTop),
            Effect::Unsupported(_) => {
                unreachable!("can_play 已过滤不可玩效果")
            }
            Effect::Damage(_, _) => unreachable!("can_play 已过滤非敌方目标伤害"),
        }
    }

    /// LouseNormal.getMove
    fn louse_get_move(&mut self, i: usize, num: i32) -> u8 {
        let h = &self.monsters[i].move_history;
        let last_bite = h.last() == Some(&3);
        let last2_bite = h.len() >= 2 && h[h.len() - 1] == 3 && h[h.len() - 2] == 3;
        let last2_grow = h.len() >= 2 && h[h.len() - 1] == 4 && h[h.len() - 2] == 4;
        if self.ascension >= 17 {
            if num < 25 {
                if last_bite {
                    3
                } else {
                    4
                }
            } else if last2_bite {
                4
            } else {
                3
            }
        } else if num < 25 {
            if last2_grow {
                3
            } else {
                4
            }
        } else if last2_bite {
            4
        } else {
            3
        }
    }

    /// FungiBeast.getMove
    fn fungi_get_move(&mut self, i: usize, num: i32) -> u8 {
        let h = &self.monsters[i].move_history;
        let last_grow = h.last() == Some(&2);
        let last2_bite = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
        if num < 60 {
            if last2_bite {
                2
            } else {
                1
            }
        } else if last_grow {
            1
        } else {
            2
        }
    }

    fn end_turn(&mut self) {
        self.turn_has_ended = true;
        // callEndOfTurnActions：玩家 powers 的 EOT，按插入序逐个结算：
        // Metallicize（PreEndTurnCards 钩子，先于其余）→ LoseStrength/Regen（atEndOfTurn）
        let mut combust_damage = 0;
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
                    // Combust：失 1 HP，对所有敌人造成 amount（THORNS 直伤，不走力量）
                    PowerKind::Combust => {
                        self.player.creature.hp -= 1;
                        combust_damage += p.amount;
                        keep.push(p);
                    }
                    // Rage：仅本回合有效，EOT 移除
                    PowerKind::Rage => {}
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
        if combust_damage > 0 {
            if self.player.creature.is_dead() {
                self.over = true;
                self.won = false;
                return;
            }
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    self.monsters[mi].creature.apply_damage(combust_damage);
                }
            }
            if self.all_monsters_dead() {
                self.over = true;
                self.won = true;
                return;
            }
        }
        // TriggerEndOfTurnOrbsAction：逐球 onEndOfTurn（被动效果）
        {
            let focus = self
                .player
                .creature
                .powers
                .iter()
                .find(|p| p.kind == PowerKind::Focus)
                .map(|p| p.amount)
                .unwrap_or(0);
            for mi in 0..self.player.orbs.len() {
                let orb = self.player.orbs[mi].clone();
                let passive = orb.passive(focus);
                match orb.kind {
                    crate::state::OrbKind::Frost => {
                        self.player.creature.block += passive;
                    }
                    crate::state::OrbKind::Lightning => {
                        // 随机敌伤害（cardRandomRng）
                        let alive: Vec<usize> = (0..self.monsters.len())
                            .filter(|&i| self.alive(i))
                            .collect();
                        if let Some(m) = alive.len().checked_sub(1) {
                            let roll = self.rng.card_random.random_start_end(0, m as i32);
                            let t = alive[(roll.max(0) as usize).min(alive.len() - 1)];
                            self.monsters[t].creature.apply_damage(passive);
                            if self.monsters[t].creature.is_dead() {
                                self.on_monster_death(t);
                            }
                        }
                    }
                    crate::state::OrbKind::Dark => {
                        // Dark.onEndOfTurn：evokeAmount += passiveAmount
                        self.player.orbs[mi].dark_growth += passive;
                    }
                    crate::state::OrbKind::Plasma => {} // Plasma 无 EOT 被动
                }
            }
        }
        // DiscardAtEndOfTurn：无保留卡（retain）与 Runic Pyramid/Equilibrium 例外
        let hand = std::mem::take(&mut self.player.hand);
        self.player.discard.extend(hand);
        // MonsterStartTurnAction（AbstractRoom:406 单次入队）：
        // 怪物回合开始时对全部存活怪统一掉格挡（非逐怪）
        for m in &mut self.monsters {
            if !m.creature.is_dead() {
                m.creature.block = 0;
            }
        }
        // Vault：跳过怪物回合
        if self.skip_monster_turn {
            self.skip_monster_turn = false;
        } else {
            // 怪物序列（MonsterGroup.queueMonsters 列表序）
            for i in 0..self.monsters.len() {
                if !self.alive(i) {
                    continue;
                }
                let mut q = self.monster_turn(i);
                q.push(Q::RollMove(i));
                self.drain(q);
                if self.over {
                    return;
                }
            }
        } // end skip_monster_turn
        self.end_of_round_powers();
        if self.over {
            return;
        }
        self.turn += 1;
        self.attacks_this_turn = 0;
        self.hand_cost_zero_this_turn = false;
        self.hand_cost_max_one = false;
        self.turn_has_ended = false;
        // ── 下回合开始序列（GameActionManager turn-start 分支）──
        // ① startOfTurnPowers（pre）：Berserk +能量；Energized（onEnergyRecharge）+能量后移除
        let mut bonus_energy = 0;
        let mut keep: Vec<Power> = Vec::new();
        for pw in self.player.creature.powers.drain(..) {
            match pw.kind {
                PowerKind::Berserk => {
                    bonus_energy += pw.amount;
                    keep.push(pw);
                }
                PowerKind::Energized => bonus_energy += pw.amount, // 移除自身
                _ => keep.push(pw),
            }
        }
        self.player.creature.powers = keep;
        // ② loseBlock（Barricade 豁免）
        if !self
            .player
            .creature
            .powers
            .iter()
            .any(|pw| pw.kind == PowerKind::Barricade)
        {
            self.player.creature.block = 0;
        }
        // ③④ 能量重置（3+加成+Plasma）→ 抽 5
        {
            let focus = self
                .player
                .creature
                .powers
                .iter()
                .find(|p| p.kind == PowerKind::Focus)
                .map(|p| p.amount)
                .unwrap_or(0);
            for orb in &self.player.orbs {
                if orb.kind == crate::state::OrbKind::Plasma {
                    bonus_energy += orb.passive(focus);
                }
            }
        }
        self.player.energy = 3 + bonus_energy; // ③ 能量重置（Plasma 加成合并）
        self.draw_cards(5);
        // Night Terror 延迟副本
        let copies = std::mem::take(&mut self.next_turn_copies);
        for (card, count) in copies {
            for _ in 0..count {
                if self.player.hand.len() < 10 {
                    self.player.hand.push(card);
                }
            }
        }
        // ⑤ atStartOfTurnPostDraw：Demon Form +力量；Brutality 抽 n 失 n 血
        let mut str_gain = 0;
        let mut draw_extra = 0;
        let mut hp_loss = 0;
        for pw in &self.player.creature.powers {
            match pw.kind {
                PowerKind::DemonForm => str_gain += pw.amount,
                PowerKind::Brutality => {
                    draw_extra += pw.amount;
                    hp_loss += pw.amount;
                }
                _ => {}
            }
        }
        if str_gain > 0 {
            apply_power(
                &mut self.player.creature.powers,
                PowerKind::Strength,
                str_gain,
                false,
                self.turn_has_ended,
            );
        }
        if draw_extra > 0 {
            self.draw_cards(draw_extra as usize);
        }
        if hp_loss > 0 {
            self.player.creature.hp -= hp_loss; // LoseHPAction 绕过格挡
            if self.player.creature.is_dead() {
                self.over = true;
                self.won = false;
            }
        }
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
            MonsterKind::LouseNormal => match m.next_move {
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                4 => q.push(Q::ApplyPowerToMonster {
                    target: i,
                    kind: PowerKind::Strength,
                    amount: m.ritual_amt, // STR_AMOUNT
                }),
                _ => {}
            },
            MonsterKind::FungiBeast => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                2 => q.push(Q::ApplyPowerToMonster {
                    target: i,
                    kind: PowerKind::Strength,
                    amount: m.ritual_amt, // strAmt
                }),
                _ => {}
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
                if self.monsters[i].creature.is_dead() {
                    self.on_monster_death(i);
                    if self.over {
                        return;
                    }
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
            if self.force_end_turn_after_queue && q.is_empty() {
                self.force_end_turn_after_queue = false;
                self.end_turn();
                return;
            }
            if self.pending.is_some() {
                // 选择屏挂起：暂存剩余队列，等待玩家响应后恢复
                self.pending_queue = Some(q);
                return;
            }
        }
    }

    fn exec(&mut self, a: Q) {
        match a {
            Q::DamageMonster { target, dmg } => {
                if self.alive(target) {
                    // Curl Up：首次受到非致死 NORMAL 伤害 → 获得 amount 格挡后移除
                    // （onAttacked 在 decrementBlock 前触发，故本次伤害即被吸收）
                    if dmg > 0 && dmg < self.monsters[target].creature.hp {
                        if let Some(idx) = self.monsters[target]
                            .creature
                            .powers
                            .iter()
                            .position(|pw| pw.kind == PowerKind::CurlUp)
                        {
                            let amt = self.monsters[target].creature.powers[idx].amount;
                            self.monsters[target].creature.powers.remove(idx);
                            self.monsters[target].creature.block += amt;
                        }
                    }
                    self.monsters[target].creature.apply_damage(dmg);
                    if self.monsters[target].creature.is_dead() {
                        self.on_monster_death(target);
                    }
                }
            }
            Q::MonsterDamage { src, base } => {
                let d = calc_monster_damage(
                    base,
                    DamageType::Normal,
                    &self.monsters[src].creature.powers,
                    &self.player.creature.powers,
                    self.player.stance,
                );
                self.player.creature.apply_damage(d);
                if self.player.creature.is_dead() {
                    self.over = true;
                    self.won = false;
                    return;
                }
                // Thorns：受击反弹（THORNS 直伤，不再触发对方荆棘/力量）
                if let Some(amt) = self
                    .player
                    .creature
                    .powers
                    .iter()
                    .find(|pw| pw.kind == PowerKind::Thorns)
                    .map(|pw| pw.amount)
                {
                    if amt > 0 && self.alive(src) {
                        self.monsters[src].creature.apply_damage(amt);
                        if self.monsters[src].creature.is_dead() {
                            self.on_monster_death(src);
                        }
                    }
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
                self.apply_power_to_player_checked(kind, amount, false);
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
            Q::DamageRandom { base } => {
                // getRandomMonster(null, true, cardRandomRng)：从存活怪物均匀抽取
                let alive: Vec<usize> = (0..self.monsters.len())
                    .filter(|&i| self.alive(i))
                    .collect();
                if !alive.is_empty() {
                    let roll = self
                        .rng
                        .card_random
                        .random_start_end(0, alive.len() as i32 - 1);
                    let k = (roll.max(0) as usize).min(alive.len() - 1);
                    let t = alive[k];
                    let d = calc_player_card_damage(
                        base,
                        DamageType::Normal,
                        &self.player.creature.powers,
                        &self.monsters[t].creature.powers,
                        self.player.stance,
                    );
                    self.monsters[t].creature.apply_damage(d);
                    if self.monsters[t].creature.is_dead() {
                        self.on_monster_death(t);
                    }
                }
            }
            Q::MakeInHand(c) => {
                // MakeTempCardInHandAction：手牌满则溢出弃牌堆
                if self.player.hand.len() < 10 {
                    self.player.hand.push(c);
                } else {
                    self.player.discard.push(c);
                }
            }
            Q::MakeInDraw { inst, random } => {
                if random {
                    // CardGroup.addToRandomSpot：cardRandomRng.random(size-1)——
                    // 种子流可对拍；索引 [0, size-1]，永不落在堆顶位（末位）
                    if self.player.draw.is_empty() {
                        self.player.draw.push(inst);
                    } else {
                        let last = self.player.draw.len() - 1;
                        let idx =
                            self.rng.card_random.random_start_end(0, last as i32).max(0) as usize;
                        self.player.draw.insert(idx.min(last), inst);
                    }
                } else {
                    self.player.draw.push(inst); // 尾 = 顶
                }
            }
            Q::RandomDiscard { n } => {
                // DiscardAction isRandom=true：getRandomCard(cardRandomRng)，逐张弃
                for _ in 0..n.max(0) {
                    let size = self.player.hand.len();
                    if size == 0 {
                        break;
                    }
                    let idx = self
                        .rng
                        .card_random
                        .random_start_end(0, size as i32 - 1)
                        .max(0) as usize;
                    let c = self.player.hand.remove(idx);
                    self.player.discard.push(c);
                }
            }
            Q::RequireHandDiscard { amount } => {
                let hand = self.player.hand.len() as i32;
                if hand <= amount {
                    // 手牌不足：自动全弃（源码第一分支）
                    let cards = std::mem::take(&mut self.player.hand);
                    self.player.discard.extend(cards);
                } else if amount < 0 {
                    self.pending = Some(Pending::HandDiscardAny);
                } else {
                    self.pending = Some(Pending::HandDiscard {
                        amount: amount as usize,
                    });
                }
            }
            Q::RequireScry { n } => {
                // 占卜非负防御 + 抽牌堆空则无事发生（源码两分支）
                let n = n.max(0) as usize;
                let cnt = n.min(self.player.draw.len());
                let len = self.player.draw.len();
                let shown = self.player.draw[len - cnt..].to_vec();
                self.pending = Some(Pending::Scry { shown });
            }
            Q::ChangeStance(new_stance) => {
                // ChangeStanceAction.update：同姿态无操作；切换前 onExit（平静→+2能量）
                if self.player.stance != new_stance {
                    if self.player.stance == crate::state::Stance::Calm {
                        self.player.energy += 2; // CalmStance.onExitStance → GainEnergy(2)
                    }
                    self.player.stance = new_stance;
                }
            }
            Q::Channel { kind, auto_evoke } => {
                // ChannelAction + AbstractPlayer.channelOrb
                if self.player.max_orbs <= 0 {
                    return; // 无槽位则不充能
                }
                if self.player.orbs.len() as i32 >= self.player.max_orbs {
                    if auto_evoke {
                        // 槽满+autoEvoke：引出最左腾位
                        self.exec(Q::EvokeLeftmost);
                    } else {
                        return; // autoEvoke=false 且满：不充能
                    }
                }
                self.player.orbs.push(crate::state::OrbState::new(kind));
            }
            Q::EvokeLeftmost => {
                // AbstractPlayer.evokeOrb: 左球 onEvoke → 移除 → 左移
                if !self.player.orbs.is_empty() {
                    let orb = self.player.orbs.remove(0);
                    self.orb_evoke_effect(&orb);
                }
            }
            Q::EvokeNoRemove => {
                if !self.player.orbs.is_empty() {
                    let orb = self.player.orbs[0].clone();
                    self.orb_evoke_effect(&orb);
                }
            }
            Q::OrbSlots(n) => {
                self.player.max_orbs = (self.player.max_orbs + n).clamp(0, 10);
            }
            Q::ShuffleDraw => {
                let mut pile = std::mem::take(&mut self.player.draw);
                card_group_shuffle(&mut pile, &mut self.rng.shuffle);
                self.player.draw = pile;
            }
            Q::DoIfStanceBlock { stance, block } => {
                if self.player.stance == stance {
                    self.player.creature.block += block;
                }
            }
            Q::DoSkipEnemyTurn => {
                // Vault：本回合怪物不行动
                self.skip_monster_turn = true;
            }
            Q::DoVampireBlock { target, base } => {
                if self.alive(target) {
                    let d = calc_player_card_damage(
                        base,
                        DamageType::Normal,
                        &self.player.creature.powers,
                        &self.monsters[target].creature.powers,
                        self.player.stance,
                    );
                    self.monsters[target].creature.apply_damage(d);
                    // Wallop：获得格挡 = 实际伤害
                    self.player.creature.block += d;
                    if self.monsters[target].creature.is_dead() {
                        self.on_monster_death(target);
                    }
                }
            }
            Q::DoExecuteEnemy { target, cutoff } => {
                if self.alive(target) && self.monsters[target].creature.hp <= cutoff {
                    self.monsters[target].creature.hp = 0;
                    self.on_monster_death(target);
                }
            }
            Q::DoGainEnergyDrawPile { divide } => {
                let n = self.player.draw.len() as i32 / divide;
                if n > 0 {
                    self.player.energy += n;
                }
            }
            Q::DoDiscardAllHand => {
                let cards = std::mem::take(&mut self.player.hand);
                self.player.discard.extend(cards);
            }
            Q::DoSetHandCostZero => {
                // BulletTime：本回合手牌全部免费（setCostForTurn(-9)）
                self.hand_cost_zero_this_turn = true;
            }
            Q::DoExhaustHand => {
                let cards = std::mem::take(&mut self.player.hand);
                let n = cards.len();
                self.player.exhaust.extend(cards);
                // onExhaust 钩子：Dark Embrace
                if let Some(de) = self
                    .player
                    .creature
                    .powers
                    .iter()
                    .find(|p| p.kind == PowerKind::DarkEmbrace)
                    .map(|p| p.amount)
                {
                    self.draw_cards(de as usize * n);
                }
            }
            Q::VampireMaxHp {
                target,
                base,
                max_hp_gain,
            } => {
                if self.alive(target) {
                    let d = calc_player_card_damage(
                        base,
                        DamageType::Normal,
                        &self.player.creature.powers,
                        &self.monsters[target].creature.powers,
                        self.player.stance,
                    );
                    self.monsters[target].creature.apply_damage(d);
                    if self.monsters[target].creature.is_dead() {
                        // FeedAction：击杀时增加最大生命
                        self.player.creature.max_hp += max_hp_gain;
                        self.player.creature.hp += max_hp_gain;
                        self.on_monster_death(target);
                    }
                }
            }
            Q::IfIntentAttack { target, str_gain } => {
                // SpotWeaknessAction：意图基伤 >= 0 时获得力量
                let has_attack_intent = self.monsters[target]
                    .move_base_damage()
                    .map(|d| d >= 0)
                    .unwrap_or(false)
                    || self.monsters[target].next_move != 2; // 非纯Buff意图
                if has_attack_intent {
                    apply_power(
                        &mut self.player.creature.powers,
                        PowerKind::Strength,
                        str_gain,
                        false,
                        false,
                    );
                }
            }
            Q::DoDiscardNonAttacks => {
                // UnloadAction：弃掉所有非攻击手牌
                let indices: Vec<usize> = self
                    .player
                    .hand
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.def.card_type != "ATTACK")
                    .map(|(i, _)| i)
                    .collect();
                for &i in indices.iter().rev() {
                    let c = self.player.hand.remove(i);
                    self.player.discard.push(c);
                }
            }
            Q::DoSelectCopyNextTurn(n) => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::CopyNextTurn,
                    });
                    self.next_turn_copy_count = n;
                }
            }
            Q::DoIfKillSelectUpgrade { target } => {
                // LessonLearned：击杀后手选升级（近似版）
                if !self.alive(target) && !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::UpgradeCard,
                    });
                }
            }
            Q::DoSelectAttackDuplicate => {
                let has_attack = self.player.hand.iter().any(|c| c.def.card_type == "ATTACK");
                if has_attack && self.player.hand.len() < 10 {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::DuplicateAttack,
                    });
                }
            }
            Q::DoSelectDrawPlayTwiceExhaust => {
                if !self.player.draw.is_empty() {
                    self.pending = Some(Pending::PilePick {
                        pile: "draw",
                        purpose: PilePickPurpose::DrawPlayTwiceExhaust,
                    });
                }
            }
            Q::DoSelectDiscardExhaust => {
                if !self.player.discard.is_empty() {
                    self.pending = Some(Pending::PilePick {
                        pile: "discard",
                        purpose: PilePickPurpose::DiscardExhaustPick,
                    });
                }
            }
            Q::DoEvokeAllOrbs => {
                while !self.player.orbs.is_empty() {
                    let orb = self.player.orbs.remove(0);
                    self.orb_evoke_effect(&orb);
                }
            }
            Q::DoTriggerAllOrbPassives => {
                let focus = self
                    .player
                    .creature
                    .powers
                    .iter()
                    .find(|p| p.kind == PowerKind::Focus)
                    .map(|p| p.amount)
                    .unwrap_or(0);
                for mi in 0..self.player.orbs.len() {
                    let orb = self.player.orbs[mi].clone();
                    let passive = orb.passive(focus);
                    match orb.kind {
                        crate::state::OrbKind::Frost => self.player.creature.block += passive,
                        crate::state::OrbKind::Lightning => {
                            let alive: Vec<usize> = (0..self.monsters.len())
                                .filter(|&i| self.alive(i))
                                .collect();
                            if let Some(m) = alive.len().checked_sub(1) {
                                let roll = self.rng.card_random.random_start_end(0, m as i32);
                                let t = alive[(roll.max(0) as usize).min(alive.len() - 1)];
                                self.monsters[t].creature.apply_damage(passive);
                                if self.monsters[t].creature.is_dead() {
                                    self.on_monster_death(t);
                                }
                            }
                        }
                        crate::state::OrbKind::Dark => self.player.orbs[mi].dark_growth += passive,
                        crate::state::OrbKind::Plasma => {}
                    }
                }
            }
            Q::DoTriggerDarkOrbPassives => {
                let focus = self
                    .player
                    .creature
                    .powers
                    .iter()
                    .find(|p| p.kind == PowerKind::Focus)
                    .map(|p| p.amount)
                    .unwrap_or(0);
                for mi in 0..self.player.orbs.len() {
                    if self.player.orbs[mi].kind == crate::state::OrbKind::Dark {
                        let passive = self.player.orbs[mi].passive(focus);
                        self.player.orbs[mi].dark_growth += passive;
                    }
                }
            }
            Q::DoIfKillGainGold { target, gold_gain } => {
                if !self.alive(target) {
                    self.gold += gold_gain;
                }
            }
            Q::DoSelectExhaustGainEnergyCost => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::ExhaustGainEnergy,
                    });
                }
            }
            Q::DoReturnZeroCostFromDiscard => {
                let indices: Vec<usize> = self
                    .player
                    .discard
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.cost_pay() == 0)
                    .rev()
                    .map(|(i, _)| i)
                    .collect();
                for i in indices {
                    if self.player.hand.len() < 10 {
                        let c = self.player.discard.remove(i);
                        self.player.hand.push(c);
                    }
                }
            }
            Q::DoSelectUpgradeCard => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::UpgradeCard,
                    });
                }
            }
            Q::DoUpgradeAllHand => {
                for c in &mut self.player.hand {
                    c.upgraded = true;
                }
            }
            Q::DoUpgradeStrikesDefends => {
                // Apotheosis：升级所有牌堆中的打击/防御
                for c in &mut self.player.draw {
                    if c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend") {
                        c.upgraded = true;
                    }
                }
                for c in &mut self.player.hand {
                    if c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend") {
                        c.upgraded = true;
                    }
                }
                for c in &mut self.player.discard {
                    if c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend") {
                        c.upgraded = true;
                    }
                }
                for c in &mut self.player.exhaust {
                    if c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend") {
                        c.upgraded = true;
                    }
                }
            }
            Q::DoBlockPerDiscard(block_per) => {
                let n = self.player.discard.len() as i32;
                self.player.creature.block += block_per * n;
            }
            Q::DoPlayTopCard => {
                // Havoc：从抽牌堆顶打出一张卡（无能量消耗）
                if !self.player.draw.is_empty() {
                    let card = self.player.draw.pop().unwrap();
                    // 执行卡牌效果（不扣能量）
                    let mut q2 = Vec::new();
                    let ti = 0;
                    for &eff in card.def.effects {
                        self.apply_effect(eff, card, ti, 0, &mut q2, 0);
                    }
                    // 去向
                    if card.def.flags.contains(&"exhaust") {
                        self.player.exhaust.push(card);
                    } else {
                        self.player.discard.push(card);
                    }
                    self.drain(q2);
                }
            }
            Q::DoSelectFreeCost => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::FreeCost,
                    });
                }
            }
            Q::DoModifySelfDamage(n) => {
                // Rampage/GlassKnife：修改弃牌堆最后一张（=刚打出的卡）的伤害
                if let Some(card) = self.player.discard.last_mut() {
                    card.damage_bonus += n;
                }
            }
            Q::DoModifySelfBlock(n) => {
                if let Some(card) = self.player.discard.last_mut() {
                    card.block_bonus += n;
                }
            }
            Q::DoModifySelfCost(n) => {
                if let Some(card) = self.player.discard.last_mut() {
                    card.cost_delta += n;
                }
            }
            Q::DoIfKillModifySelfDamage {
                target,
                damage_gain,
            } => {
                // RitualDagger：击杀时永久增加本卡伤害
                if !self.alive(target) {
                    if let Some(card) = self.player.discard.last_mut() {
                        card.damage_bonus += damage_gain;
                    }
                }
            }
            Q::DoNextTurnDraw(n) => {
                self.next_turn_draw += n;
            }
            Q::DoNextTurnGainEnergy(n) => {
                self.next_turn_energy += n;
            }
            Q::DoSetHandCostOne => {
                // Enlightenment：本回合手牌费用上限 1
                self.hand_cost_max_one = true;
            }
            Q::DoIfNoAttacksDraw(n) => {
                // Impatience：手牌无攻击牌时抽 N 张
                let has_attack = self.player.hand.iter().any(|c| c.def.card_type == "ATTACK");
                if !has_attack {
                    self.draw_cards(n as usize);
                }
            }
            Q::DoIfKillGainEnergy {
                target,
                energy_gain,
            } => {
                // Sunder：目标已死（在前序 Damage 中被击杀）则获得能量
                if !self.alive(target) {
                    self.player.energy += energy_gain;
                }
            }
            Q::DoMoveTypeToHand(card_type) => {
                // Violence/SecretTechnique/SecretWeapon：将抽牌堆中指定类型全移入手
                let matching: Vec<usize> = self
                    .player
                    .draw
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.def.card_type == card_type)
                    .rev()
                    .map(|(i, _)| i)
                    .collect();
                for i in matching {
                    if self.player.hand.len() < 10 {
                        let c = self.player.draw.remove(i);
                        self.player.hand.push(c);
                    }
                }
            }
            Q::DoRemoveAllBlock { target } => {
                if self.alive(target) {
                    self.monsters[target].creature.block = 0;
                }
            }

            Q::DoSelectDrawPileToHand => {
                if !self.player.draw.is_empty() && self.player.hand.len() < 10 {
                    self.pending = Some(Pending::PilePick {
                        pile: "draw",
                        purpose: PilePickPurpose::DrawPileToHand,
                    });
                }
            }
            Q::DoIfLastCardSkill(n) => {
                if self.last_card_type.as_deref() == Some("SKILL") {
                    self.draw_cards(n as usize);
                }
            }
            Q::DoDrawToHand(target) => {
                // ExpertiseAction：抽到手牌有 target 张
                let to_draw = (target - self.player.hand.len() as i32).max(0);
                if to_draw > 0 {
                    self.draw_cards(to_draw as usize);
                }
            }
            Q::DoIfIntentApplyWeak { target, weak_amt } => {
                // ForTheEyes：意图含攻击时施加虚弱
                if self.alive(target)
                    && self.monsters[target]
                        .move_base_damage()
                        .map(|d| d >= 0)
                        .unwrap_or(false)
                {
                    apply_power(
                        &mut self.monsters[target].creature.powers,
                        PowerKind::Weak,
                        weak_amt,
                        false,
                        self.turn_has_ended,
                    );
                }
            }
            Q::DoSelectDiscardToHand => {
                if !self.player.discard.is_empty() && self.player.hand.len() < 10 {
                    self.pending = Some(Pending::PilePick {
                        pile: "discard",
                        purpose: PilePickPurpose::DiscardToHand,
                    });
                }
            }
            Q::DoSelectPutOnDeckTop => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::PutOnDeckTop,
                    });
                }
            }
            Q::DoSelectPutOnDeckBottom => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandPick {
                        purpose: HandPickPurpose::PutOnDeckBottom,
                    });
                }
            }
            Q::DoSelectExhaustToHand => {
                if !self.player.exhaust.is_empty() && self.player.hand.len() < 10 {
                    self.pending = Some(Pending::PilePick {
                        pile: "exhaust",
                        purpose: PilePickPurpose::ExhaustToHand,
                    });
                }
            }
            Q::DoSelectDiscardToTop => {
                if !self.player.discard.is_empty() {
                    self.pending = Some(Pending::PilePick {
                        pile: "discard",
                        purpose: PilePickPurpose::DiscardToDeckTop,
                    });
                }
            }
            Q::DoIfDiscardedGainEnergy(n) => {
                if self.discarded_this_turn > 0 {
                    self.player.energy += n;
                }
            }
            Q::GainBlock(n) => {
                self.player.creature.block += n;
            }
            Q::ForceEndTurn => {
                self.force_end_turn_after_queue = true;
            }
            Q::VampireDamageAll { base } => {
                let mut total = 0i32;
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        let d = calc_player_card_damage(
                            base,
                            DamageType::Normal,
                            &self.player.creature.powers,
                            &self.monsters[mi].creature.powers,
                            self.player.stance,
                        );
                        self.monsters[mi].creature.apply_damage(d);
                        total += d;
                        if self.monsters[mi].creature.is_dead() {
                            self.on_monster_death(mi);
                        }
                    }
                }
                if total > 0 {
                    self.player.creature.hp =
                        (self.player.creature.hp + total).min(self.player.creature.max_hp);
                }
            }
            Q::ModifyPowerMonster { target, kind, mult } => {
                if self.alive(target) {
                    if let Some(p) = self.monsters[target]
                        .creature
                        .powers
                        .iter_mut()
                        .find(|p| p.kind == kind)
                    {
                        p.amount *= mult;
                    }
                }
            }
            Q::ModifyPowerSelf { kind, mult } => {
                if let Some(p) = self
                    .player
                    .creature
                    .powers
                    .iter_mut()
                    .find(|p| p.kind == kind)
                {
                    p.amount *= mult;
                }
            }
            Q::DamageIfPoisoned { target, base } => {
                if self.alive(target)
                    && self.monsters[target]
                        .creature
                        .powers
                        .iter()
                        .any(|p| p.kind == PowerKind::Poison)
                {
                    let d = calc_player_card_damage(
                        base,
                        DamageType::Normal,
                        &self.player.creature.powers,
                        &self.monsters[target].creature.powers,
                        self.player.stance,
                    );
                    self.monsters[target].creature.apply_damage(d);
                    if self.monsters[target].creature.is_dead() {
                        self.on_monster_death(target);
                    }
                }
            }
            Q::ExhaustNonAttacks { block_per } => {
                let indices: Vec<usize> = self
                    .player
                    .hand
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.def.card_type != "ATTACK")
                    .map(|(i, _)| i)
                    .collect();
                for &i in indices.iter().rev() {
                    let c = self.player.hand.remove(i);
                    self.player.exhaust.push(c);
                    self.player.creature.block += block_per;
                }
            }
            Q::DamagePerAttack { target, base } => {
                let count = (self.attacks_this_turn - 1).max(0);
                for _ in 0..count {
                    if self.alive(target) {
                        let d = calc_player_card_damage(
                            base,
                            DamageType::Normal,
                            &self.player.creature.powers,
                            &self.monsters[target].creature.powers,
                            self.player.stance,
                        );
                        self.monsters[target].creature.apply_damage(d);
                        if self.monsters[target].creature.is_dead() {
                            self.on_monster_death(target);
                            break;
                        }
                    }
                }
            }
            Q::RollMove(i) => self.roll_move(i),
        }
    }

    /// 球 onEvoke 效果（orbs/*.java）
    fn orb_evoke_effect(&mut self, orb: &crate::state::OrbState) {
        let focus = self
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Focus)
            .map(|p| p.amount)
            .unwrap_or(0);
        let evoke = orb.evoke(focus);
        match orb.kind {
            crate::state::OrbKind::Frost => {
                self.player.creature.block += evoke; // GainBlockAction
            }
            crate::state::OrbKind::Lightning => {
                // LightningOrbEvokeAction：随机敌（getRandomMonster → cardRandomRng 可对拍）
                let alive: Vec<usize> = (0..self.monsters.len())
                    .filter(|&i| self.alive(i))
                    .collect();
                if let Some(roll) = alive
                    .len()
                    .checked_sub(1)
                    .map(|m| self.rng.card_random.random_start_end(0, m as i32))
                {
                    let t = alive[(roll.max(0) as usize).min(alive.len() - 1)];
                    self.monsters[t].creature.apply_damage(evoke);
                    if self.monsters[t].creature.is_dead() {
                        self.on_monster_death(t);
                    }
                }
            }
            crate::state::OrbKind::Dark => {
                // DarkOrbEvokeAction：对所有敌人造成伤害（THORNS 类型）
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        self.monsters[mi].creature.apply_damage(evoke);
                    }
                }
                if self.all_monsters_dead() {
                    self.over = true;
                    self.won = true;
                }
            }
            crate::state::OrbKind::Plasma => {
                self.player.energy += evoke; // GainEnergyAction
            }
        }
    }

    /// 怪物死亡钩子：Spore Cloud（对玩家施加易伤）→ 全灭判定
    fn on_monster_death(&mut self, i: usize) {
        if let Some(idx) = self.monsters[i]
            .creature
            .powers
            .iter()
            .position(|pw| pw.kind == PowerKind::SporeCloud)
        {
            let amt = self.monsters[i].creature.powers[idx].amount;
            self.monsters[i].creature.powers.remove(idx);
            self.apply_power_to_player_checked(PowerKind::Vulnerable, amt, true);
        }
        if self.all_monsters_dead() {
            self.over = true;
            self.won = true;
        }
    }

    /// 对玩家施加 power：神器（Artifact）可抵消减益（Vulnerable/Weak/Frail/Poison）
    pub(crate) fn apply_power_to_player_checked(
        &mut self,
        kind: PowerKind,
        amount: i32,
        source_is_monster: bool,
    ) {
        // ApplyPowerAction:125 —— 按 powerToApply.type == DEBUFF 判定，非硬编码清单
        if kind.is_debuff() {
            if let Some(idx) = self
                .player
                .creature
                .powers
                .iter()
                .position(|pw| pw.kind == PowerKind::Artifact)
            {
                if self.player.creature.powers[idx].amount > 0 {
                    self.player.creature.powers[idx].amount -= 1;
                    if self.player.creature.powers[idx].amount == 0 {
                        self.player.creature.powers.remove(idx);
                    }
                    return; // 被神器抵消
                }
            }
        }
        apply_power(
            &mut self.player.creature.powers,
            kind,
            amount,
            source_is_monster,
            self.turn_has_ended,
        );
    }

    /// DrawCardAction + EmptyDeckShuffleAction（源码核对语义）：
    /// - hand==10 → 本轮抽牌全部作废；amount+hand>10 → amount 截断（多余消失，不弃牌）
    /// - 抽牌堆空且弃牌堆非空 → 洗回（shuffleRng）后继续
    /// - 牌堆全空 → 停止
    pub fn draw_cards(&mut self, n: usize) {
        if self.player.hand.len() >= 10 {
            return; // createHandIsFullDialog：完全停止
        }
        let space = 10 - self.player.hand.len();
        let mut left = n.min(space); // 截断到 10
        while left > 0 {
            if self.player.draw.is_empty() {
                if self.player.discard.is_empty() {
                    break;
                }
                let mut pile = std::mem::take(&mut self.player.discard);
                card_group_shuffle(&mut pile, &mut self.rng.shuffle);
                self.player.draw = pile;
            }
            let c = self.player.draw.pop().unwrap();
            self.player.hand.push(c);
            left -= 1;
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
            MonsterKind::LouseNormal => self.louse_get_move(i, num),
            MonsterKind::FungiBeast => self.fungi_get_move(i, num),
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
        self.player.stance.hash(h);
        self.player.max_orbs.hash(h);
        for orb in &self.player.orbs {
            (orb.kind, orb.dark_growth).hash(h);
        }
        self.player.creature.block.hash(h);
        self.player.energy.hash(h);
        for c in &self.player.hand {
            (c.def.id, c.upgraded).hash(h);
        }
        for c in &self.player.discard {
            (c.def.id, c.upgraded).hash(h);
        }
        for m in &self.monsters {
            m.kind.hash(h);
            m.creature.hp.hash(h);
            m.creature.block.hash(h);
            m.next_move.hash(h);
            m.move_history.hash(h);
            for p in &m.creature.powers {
                (p.id(), p.amount, p.just_applied).hash(h);
            }
        }
        self.turn.hash(h);
        match &self.pending {
            None => 0u8.hash(h),
            Some(Pending::HandDiscard { amount }) => {
                1u8.hash(h);
                amount.hash(h);
            }
            Some(Pending::HandDiscardAny) => 2u8.hash(h),
            Some(Pending::Scry { shown }) => {
                3u8.hash(h);
                for c in shown {
                    (c.def.id, c.upgraded).hash(h);
                }
            }
            Some(Pending::HandPick { purpose }) => {
                4u8.hash(h);
                purpose.hash(h);
            }
            Some(Pending::PilePick { pile, .. }) => {
                5u8.hash(h);
                pile.hash(h);
            }
        }
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
        assert!(n >= 115, "数据驱动出牌应达到当前硬上限附近，当前 {n}");
    }

    /// 默认状态 = 全解锁 + A20：起始卡组含进阶之灾、玩家 72/80 HP、大颚虫 A17 档数值
    #[test]
    fn a20_default_state() {
        let c = Combat::new(1);
        assert_eq!(c.ascension, DEFAULT_ASCENSION);
        assert!(c
            .player
            .draw
            .iter()
            .chain(c.player.hand.iter())
            .any(|x| x.def.id == "AscendersBane"));
        assert_eq!(c.player.creature.hp, 72); // round(80×0.9)，A6 起始 90%
        assert_eq!(c.player.creature.max_hp, 80);
        assert!(c.monsters[0].creature.max_hp >= 42 && c.monsters[0].creature.max_hp <= 46);
        assert_eq!(c.monsters[0].chomp_dmg, 12); // A17 档
        assert_eq!(c.monsters[0].bellow_str, 5);
        assert_eq!(c.monsters[0].bellow_block, 9);
    }

    #[test]
    fn bash_vulnerable_then_strike() {
        let mut c = Combat::new_asc(42, 0);
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

    /// 双重打击：两张打击的伤害（Twin Strike 2×5? 打击G=5？铁甲 Twin Strike 基伤 5×2）
    #[test]
    fn twin_strike_hits_twice() {
        let mut c = Combat::new_asc(4, 0);
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

    /// 顺劈斩：双大颚虫各吃 8
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

    /// 金刚臂：伤害 + 虚弱 2 层
    #[test]
    fn clothesline_applies_weak() {
        let mut c = Combat::new_asc(6, 0);
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

    /// 活动肌肉：+2 力量，回合末失效
    #[test]
    fn flex_temporary_strength() {
        let mut c = Combat::new_asc(3, 0);
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
            "回合末活动肌肉力量应失效"
        );
    }

    /// 全身撞击：伤害 = 当前格挡（先防御 5 再猛击 5）
    #[test]
    fn body_slam_uses_block() {
        let mut c = Combat::new_asc(5, 0);
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

    /// 愤怒：打出后弃牌堆多一张复制
    #[test]
    fn anger_copies_to_discard() {
        let mut c = Combat::new_asc(8, 0);
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

    /// 剑柄打击：伤害 + 抽 1
    #[test]
    fn pommel_strike_draws() {
        let mut c = Combat::new_asc(2, 0);
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
            let mut c = Combat::new_asc(seed, 0);
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
        let mut a = Combat::new_asc(21, 0);
        a.step(&Action::EndTurn);
        let pub0 = a.public_hash();
        let mut w1 = a.clone();
        w1.determinize(1001);
        let mut w2 = a.clone();
        w2.determinize(2002);
        assert_eq!(w1.public_hash(), pub0);
        assert_eq!(w2.public_hash(), pub0);
    }

    /// 旋风斩：X 费支付全部能量（3 能量 → 打 3 次全场 5 伤）
    #[test]
    fn whirlwind_x_cost_repeat() {
        let mut c = Combat::new_with(9, 0, &[MonsterKind::JawWorm, MonsterKind::JawWorm]);
        let (hp0, hp1) = (c.monsters[0].creature.hp, c.monsters[1].creature.hp);
        force_hand(&mut c, &["Whirlwind"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.energy, 0, "X 费支付全部能量");
        assert_eq!(hp0 - c.monsters[0].creature.hp, 15, "5×3");
        assert_eq!(hp1 - c.monsters[1].creature.hp, 15);
    }

    /// 杂技（Acrobatics）：抽2后挂起弃1选择屏，响应后恢复
    #[test]
    fn hand_select_flow() {
        let mut c = Combat::new_asc(1, 0);
        c.player.draw = vec![
            CardInst::new("Defend_R"),
            CardInst::new("Strike_R"),
            CardInst::new("Strike_R"),
        ];
        c.player.hand = vec![CardInst::new("Acrobatics")];
        c.player.discard.clear();
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        assert!(matches!(
            c.pending,
            Some(Pending::HandDiscard { amount: 1 })
        ));
        assert_eq!(c.player.hand.len(), 3, "抽3（源码2022平衡后 baseMagic=3）");
        c.step(&Action::HandSelect { indices: vec![0] });
        assert!(c.pending.is_none());
        assert_eq!(c.player.hand.len(), 2);
        assert_eq!(c.player.discard.len(), 2, "本体 + 弃掉的1张");
    }

    /// 第三只眼（Third Eye）：格挡后占卜，弃1其余保持原序置顶
    #[test]
    fn scry_flow() {
        let mut c = Combat::new_asc(2, 0);
        c.player.draw = vec![
            CardInst::new("Defend_R"), // 底
            CardInst::new("Strike_R"), // shown idx0
            CardInst::new("Strike_R"), // shown idx1
            CardInst::new("Defend_R"), // shown idx2 = 顶
        ];
        c.player.hand = vec![CardInst::new("ThirdEye")];
        c.player.discard.clear();
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        let shown = match c.pending.clone() {
            Some(Pending::Scry { shown }) => shown,
            _ => panic!("应有占卜屏"),
        };
        assert_eq!(shown.len(), 3, "占卜3");
        let len_before = c.player.draw.len();
        c.step(&Action::ScryChoice {
            discard_indices: vec![0],
        });
        assert!(c.pending.is_none());
        assert_eq!(c.player.draw.len(), len_before - 1);
        assert_eq!(c.player.discard.len(), 2, "本体 + 弃1");
        // 剩余两张保持原序：顶=Defend、次顶=Strike
        let len = c.player.draw.len();
        assert_eq!(c.player.draw[len - 1].def.id, "Defend_R");
        assert_eq!(c.player.draw[len - 2].def.id, "Strike_R");
    }

    /// 随机弃牌（All Out Attack 类）：无选择屏，cardRandomRng 确定
    #[test]
    fn random_discard_deterministic() {
        let run = |seed: u64| -> usize {
            let mut c = Combat::new_asc(seed, 0);
            let def = CARDS
                .iter()
                .find(|d| {
                    d.effects
                        .iter()
                        .any(|e| matches!(e, Effect::HandDiscard(_, true)))
                        && engine_playable(d)
                })
                .expect("存在随机弃牌卡");
            c.player.hand = vec![CardInst {
                def,
                upgraded: false,
                damage_bonus: 0,
                block_bonus: 0,
                cost_delta: 0,
            }];
            for _ in 0..3 {
                c.player.hand.push(CardInst::new("Strike_R"));
            }
            c.player.discard.clear();
            c.step(&Action::PlayCard {
                index: 0,
                target: None,
            });
            assert!(c.pending.is_none(), "随机弃牌无选择屏");
            c.player.discard.len()
        };
        assert_eq!(run(7), run(7), "同种子确定");
        assert_eq!(run(7), 2, "本体 + 随机弃1");
    }

    fn pamount(c: &Combat, k: PowerKind) -> Option<i32> {
        c.player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == k)
            .map(|p| p.amount)
    }

    /// 回合开始 power：Berserk/Energized 加能量、Energized 移除
    #[test]
    fn start_of_turn_energy_powers() {
        let mut c = Combat::new_asc(1, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Berserk,
            2,
            false,
            false,
        );
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Energized,
            2,
            false,
            false,
        );
        c.step(&Action::EndTurn);
        assert_eq!(c.player.energy, 7, "3 + Berserk2 + Energized2");
        assert!(pamount(&c, PowerKind::Energized).is_none());
        assert_eq!(pamount(&c, PowerKind::Berserk), Some(2));
    }

    /// 恶魔形态：每回合开始 +2 力量
    #[test]
    fn demon_form_grows() {
        let mut c = Combat::new_asc(2, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::DemonForm,
            2,
            false,
            false,
        );
        c.step(&Action::EndTurn);
        assert_eq!(pamount(&c, PowerKind::Strength), Some(2));
        c.step(&Action::EndTurn);
        assert_eq!(pamount(&c, PowerKind::Strength), Some(4));
    }

    /// 荆棘：受击反弹 3，THORNS 直伤
    #[test]
    fn thorns_reflects() {
        let mut c = Combat::new_asc(3, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Thorns,
            3,
            false,
            false,
        );
        let hp0 = c.monsters[0].creature.hp;
        c.monsters[0].next_move = 1; // 咬噬 11
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, 80 - 11);
        assert_eq!(c.monsters[0].creature.hp, hp0 - 3);
    }

    /// 无形：FinalReceive 管线把伤害截为 1
    #[test]
    fn intangible_caps_damage() {
        let mut c = Combat::new_asc(4, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::IntangiblePlayer,
            1,
            false,
            false,
        );
        c.monsters[0].next_move = 1;
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 1);
    }

    /// 神器：抵消一次减益后消失
    #[test]
    fn artifact_negates_debuff() {
        let mut c = Combat::new_asc(5, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Artifact,
            1,
            false,
            false,
        );
        c.apply_power_to_player_checked(PowerKind::Vulnerable, 2, true);
        assert!(pamount(&c, PowerKind::Vulnerable).is_none());
        assert!(pamount(&c, PowerKind::Artifact).is_none());
        c.apply_power_to_player_checked(PowerKind::Vulnerable, 2, true);
        assert_eq!(pamount(&c, PowerKind::Vulnerable), Some(2));
    }

    /// 壁垒：回合开始保留剩余格挡
    #[test]
    fn barricade_retains_block() {
        let mut c = Combat::new_asc(6, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Barricade,
            -1,
            false,
            false,
        );
        force_hand(&mut c, &["Defend_R", "Defend_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        }); // 格挡 10
        c.monsters[0].next_move = 3; // 抽打 7
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.block, 3, "壁垒保留 10-7=3");
    }

    /// 燃烧：玩家 EOT 失 1 HP，对所有敌人造成 5（此时咆哮尚未发生、无格挡，直接掉血）
    #[test]
    fn combust_eot() {
        let mut c = Combat::new_asc(7, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Combust,
            5,
            false,
            false,
        );
        let hp0 = c.monsters[0].creature.hp;
        let php0 = c.player.creature.hp;
        c.monsters[0].next_move = 2; // 咆哮不攻击，+6 格挡
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, php0 - 1);
        assert_eq!(c.monsters[0].creature.hp, hp0 - 5);
        assert_eq!(c.monsters[0].creature.block, 6, "咆哮在燃烧后结算");
    }

    /// 怒火：每张攻击牌 +3 格挡，非攻击不加，EOT 移除
    #[test]
    fn rage_blocks_per_attack() {
        let mut c = Combat::new_asc(8, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Rage,
            3,
            false,
            false,
        );
        force_hand(&mut c, &["Strike_R", "Defend_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.player.creature.block, 3);
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.creature.block, 8);
        c.step(&Action::EndTurn);
        assert!(pamount(&c, PowerKind::Rage).is_none());
    }

    /// 暗之拥：消耗一张牌抽一张
    #[test]
    fn dark_embrace_draws_on_exhaust() {
        let mut c = Combat::new_asc(9, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::DarkEmbrace,
            1,
            false,
            false,
        );
        let def = CARDS
            .iter()
            .find(|d| d.flags.contains(&"exhaust") && engine_playable(d))
            .expect("存在可玩消耗牌");
        let old = std::mem::take(&mut c.player.hand);
        c.player.draw.extend(old);
        c.player.hand = vec![CardInst {
            def,
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        }];
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        assert!(c.player.exhaust.iter().any(|x| x.def.id == def.id));
        assert_eq!(c.player.hand.len(), 1, "消耗1张+暗之拥抽1张");
    }

    /// 充能球完整流程：充能→EOT被动→引出→槽满自动引出
    #[test]
    fn orb_system_flow() {
        let mut c = Combat::new_asc(1, 0);
        // 闪电充能 → EOT 被动伤 → 引出伤
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Lightning,
            auto_evoke: true,
        });
        assert_eq!(c.player.orbs.len(), 1);
        assert_eq!(c.player.orbs[0].kind, crate::state::OrbKind::Lightning);
        let hp0 = c.monsters[0].creature.hp;
        // EOT：闪电被动 3 伤（randomMonster → 单怪=必中）
        c.end_turn();
        assert_eq!(c.monsters[0].creature.hp, hp0 - 3, "闪电EOT被动3伤");
        // 下回合引出（8伤）
        let hp1 = c.monsters[0].creature.hp;
        c.exec(Q::EvokeLeftmost);
        assert_eq!(c.monsters[0].creature.hp, hp1 - 8, "闪电引出8伤");
        assert!(c.player.orbs.is_empty(), "引出后槽空");
    }

    /// 霜球：EOT被动格挡 + 引出格挡
    #[test]
    fn frost_orb_block() {
        let mut c = Combat::new_asc(2, 0);
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Frost,
            auto_evoke: true,
        });
        // 有霜球：EOT +2 格挡 → 咬噬11被吸收2 → 失9
        // 无霜球对照：咬噬11 → 失11
        c.monsters[0].next_move = 1; // 咬噬
        let hp_with_orb;
        {
            let mut c2 = c.clone();
            c2.step(&Action::EndTurn);
            hp_with_orb = c2.player.creature.hp;
        }
        // 无球对照
        c.player.orbs.clear();
        c.step(&Action::EndTurn);
        let hp_without = c.player.creature.hp;
        assert_eq!(hp_with_orb - hp_without, 2, "有霜球少扣2血（格挡吸收）");
    }

    /// 暗球：EOT 自增长 + 引出全场伤
    #[test]
    fn dark_orb_growth() {
        let mut c = Combat::new_with(3, 0, &[MonsterKind::JawWorm, MonsterKind::JawWorm]);
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Dark,
            auto_evoke: true,
        });
        assert_eq!(c.player.orbs[0].dark_growth, 0);
        // EOT：dark_growth += passive(6)
        let block_before = c.player.creature.block;
        c.monsters[0].next_move = 2; // 咆哮不攻击
        c.step(&Action::EndTurn);
        assert_eq!(c.player.orbs[0].dark_growth, 6, "EOT 自增 +6");
        assert_eq!(block_before, c.player.creature.block, "暗球EOT不加格挡");
        // 直接验证 evoke 数值
        let orb = &c.player.orbs[0];
        assert_eq!(orb.evoke(0), 12, "6 base + 6 growth = 12");
        // 槽满 evict 验证：充能第 2 球（霜球）
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Frost,
            auto_evoke: true,
        });
    }

    /// 等离子球：回合开始 +2 能量
    #[test]
    fn plasma_orb_energy() {
        let mut c = Combat::new_asc(4, 0);
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Plasma,
            auto_evoke: true,
        });
        c.step(&Action::EndTurn);
        // 下回合：3 + Plasma(2) = 5 能量
        assert_eq!(c.player.energy, 4, "3 + Plasma passive 1");
    }

    /// 槽满 autoEvoke=true：引出最左腾位
    #[test]
    fn orb_full_autoevoke() {
        let mut c = Combat::new_asc(5, 0);
        c.player.max_orbs = 2;
        for _ in 0..3 {
            c.exec(Q::Channel {
                kind: crate::state::OrbKind::Frost,
                auto_evoke: true,
            });
        }
        assert_eq!(c.player.orbs.len(), 2, "槽满");
        // 第 4 次 autoEvoke=true → 引出最左（Frost 引出=5格挡），再填入
        let block0 = c.player.creature.block;
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Lightning,
            auto_evoke: true,
        });
        assert_eq!(c.player.orbs.len(), 2, "仍满（引出1+充能1）");
        assert_eq!(c.player.creature.block, block0 + 5, "引出最左霜球 +5 格挡");
        assert_eq!(c.player.orbs[0].kind, crate::state::OrbKind::Frost);
        assert_eq!(c.player.orbs[1].kind, crate::state::OrbKind::Lightning);
    }

    /// 专注：被动/引出 +N（Plasma 例外）
    #[test]
    fn focus_applies_to_orbs() {
        let mut c = Combat::new_asc(6, 0);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Focus,
            2,
            false,
            false,
        );
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Frost,
            auto_evoke: true,
        });
        // 有效 passive = 2+2=4；有效 evoke = 5+2=7
        let _block0 = c.player.creature.block;
        // EOT 被动
        c.monsters[0].next_move = 2;
        c.step(&Action::EndTurn);
        // 引出
        let block_before_ev = c.player.creature.block;
        c.exec(Q::EvokeLeftmost);
        assert_eq!(
            c.player.creature.block,
            block_before_ev + 7,
            "引出 = 5+2专注 = 7"
        );
    }

    /// 虱子：蜷缩触发（首次受到非致死伤害时获得格挡）
    #[test]
    fn louse_curl_up_triggers() {
        let mut c = Combat::new_with(1, 0, &[MonsterKind::LouseNormal]);
        // 蜷缩值在 3-7 范围内
        let curl = c.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::CurlUp)
            .map(|p| p.amount)
            .expect("虱子战前应有蜷缩");
        assert!((3..=7).contains(&curl));
        // 打击触发蜷缩
        let hp0 = c.monsters[0].creature.hp;
        force_hand(&mut c, &["Strike_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        let hp_loss = hp0 - c.monsters[0].creature.hp;
        assert_eq!(hp_loss, (6 - curl).max(0), "6伤-蜷缩{curl}");
        assert!(!c.monsters[0]
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::CurlUp));
    }

    /// 菌兽：死亡时孢子云施加易伤
    #[test]
    fn fungi_beast_spore_cloud_on_death() {
        let mut c = Combat::new_with(2, 0, &[MonsterKind::FungiBeast]);
        // 菌兽血量 22-28，打击 ×4（6×4=24 伤，可能杀不死）
        // 直接设血量为 5 确保一击致死
        c.monsters[0].creature.hp = 5;
        c.monsters[0].creature.max_hp = 5;
        force_hand(&mut c, &["Strike_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert!(c.over, "菌兽死亡");
        assert!(c.won);
        // 孢子云：死亡时对玩家施加 2 层易伤
        assert!(
            pamount(&c, PowerKind::Vulnerable).is_some(),
            "孢子云应施加易伤"
        );
    }

    /// 菌兽生长：每回合开始 AI 概率决策
    #[test]
    fn fungi_beast_bite_and_grow() {
        let mut c = Combat::new_with(3, 0, &[MonsterKind::FungiBeast]);
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        let dmg_taken = hp0 - c.player.creature.hp;
        // 菌兽招式：咬（6伤）或生长（+3力），或两者都可能
        assert!(
            dmg_taken == 0 || dmg_taken == 6,
            "咬 6 或生长 0，实际 {dmg_taken}"
        );
        // 如果是生长，怪物应有力量
        if dmg_taken == 0 {
            assert!(
                c.monsters[0]
                    .creature
                    .powers
                    .iter()
                    .any(|p| p.kind == PowerKind::Strength),
                "生长应给力量"
            );
        }
    }
}
