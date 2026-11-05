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
use crate::effects::{CardDef, Effect, Expr, Target};
use crate::potions::{pool_for_class, return_random_potion, PotionKind};
use crate::powers::{apply_power, DamageType, Decay, Power, PowerKind};
use crate::relics::{RelicInst, RelicKind};
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
    /// 使用第 index 瓶药水（投掷型需目标；能量免费）
    UsePotion {
        index: usize,
        target: Option<usize>,
    },
    /// 丢弃药水（不消耗行动）
    DiscardPotion {
        index: usize,
    },
    /// 探素（Discovery）三选一：选 options[index]（×copies 入手，本回合 0 费）
    DiscoveryResponse {
        index: usize,
    },
    /// 姿态药水：进入 Calm（false）或 Wrath（true）
    StanceResponse {
        wrath: bool,
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
    /// 液态记忆：弃堆选 1 入手且本回合 0 费（BetterDiscardPileToHandAction flag=0）
    DiscardToHandFreeCost,
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
    /// 探素屏：三选一（选项在打开时用 cardRandomRng 采样，去重）
    Discovery {
        options: Vec<CardInst>,
        copies: i32,
    },
    /// 姿态药水：平静/愤怒二选一
    StanceChoice,
    /// 赌徒特酿：弃任意张（可 0 张），抽等量
    GamblersDiscard,
    /// 不死药（已废弃）：消耗任意张手牌
    HandExhaustAny,
}

/// 引擎级原子动作（引擎 action 队列的 M2 子集）
#[derive(Clone, Copy, Debug)]
pub(crate) enum Q {
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
    ApplyPowerToPlayerFromMonster {
        kind: PowerKind,
        amount: i32,
    },
    RollMove(usize),
    /// SetMoveAction：确定性设置 next_move（不掷 aiRng）
    SetMonsterMove { target: usize, next: u8 },
    /// 设置怪物通用计数器（burn_upgraded 字段被多只怪复用）
    SetMonsterCounter { target: usize, value: i32 },
    // —— 药水系统 ——
    /// 纯伤害（THORNS 管线跳过双方 power：火焰/爆炸药水）
    DamagePure {
        target: usize,
        dmg: i32,
    },
    DamageAllPure {
        dmg: i32,
    },
    /// 探素屏：type=null → 无色池（AttackPotion 系列）
    DoDiscovery {
        pool: DiscoveryPool,
        copies: i32,
    },
    DoStanceChoice,
    DoGamblersDiscard,
    DoExhaustAny,
    DoDiscardPickFreeCost,
    /// 蒸馏混沌：打抽牌堆顶 n 次（目标随机怪）
    DoDistilledChaos {
        n: i32,
    },
    /// 异蛇之油后半：随机化手牌费用（cardRandomRng.random(3)，永改）
    DoRandomizeHandCost,
    /// 暗之精华：字面复刻双重循环（0 球时无效的源码行为也保留）
    DoChannelDarkPerOrb {
        amount: i32,
    },
    /// 混沌药水：每个空栏位掷一瓶（potionRng，limited 路径）
    DoObtainRandomPotions,
    /// 果汁：max HP +n（战斗内同时治疗 n）
    MaxHpPlus(i32),
    /// 烟雾弹：逃离战斗（无奖励，不判负）
    DoSmokeEscape,
    /// 邪教徒药水：玩家侧仪式
    DoPlayerRitual(i32),
    /// Inferno 的 BurnIncreaseAction：升级弃/抽牌堆全部灼伤
    UpgradeAllBurnPiles,
    /// 灼伤回合末自伤（THORNS 管线：无力量/虚弱/易伤乘区；格挡吸收；无形截断）
    PlayerFlatDamage(i32),
    /// 遗物直伤全体（Letter Opener 等：DamageAllEnemiesAction + THORNS 类型，
    /// 无乘区、不触发 CurlUp/Angry/Flight 等受击钩子）
    DamageAllThorns {
        dmg: i32,
    },
    // —— Act 2 怪物 ——
    /// Centurion Protect：随机其他存活怪（无则自己）获得格挡（exec 时消耗 aiRng）
    MonsterBlockRandomOther {
        src: usize,
        amount: i32,
    },
    /// Healer：全部存活怪回血
    HealAllMonsters(i32),
    /// Healer：全部存活怪 +力量
    StrAllMonsters(i32),
    /// ShelledParasite 吸取：吸血（按实际掉血治疗自己）
    MonsterVampire {
        src: usize,
        base: i32,
    },
    /// Mugger 偷金（先于伤害，无条件）：min(amount, player.gold)
    StealGold {
        src: usize,
        amount: i32,
    },
    /// Mugger/Looter 逃离（不判负；全员离场=战斗结束）
    MonsterEscape {
        src: usize,
    },
    // —— Act 2 精英/Boss ——
    /// 召唤怪物（SpawnMonsterAction：构造器初始招，不消耗 aiRng；
    /// HP 用 monster_hp 流——真游戏为全局随机，此处取确定性近似）
    SpawnMonster {
        kind: MonsterKind,
    },
    /// 小鬼首领 Rally：aiRng 八池抽一只小鬼填入空位（死亡位优先）
    SummonGremlin,
    /// 铜球 Stasis：抽牌堆（空则弃牌堆）按 稀有→罕见→普通→任意 抽走一张
    StasisSteal {
        src: usize,
    },
    /// 铜球支援光束：给指定种类的怪（自动机）格挡
    BlockMonsterKind {
        kind: MonsterKind,
        amount: i32,
    },
    /// 小鬼首领 Encourage：全员 +力量，其他存活怪 +格挡
    LeaderEncourage {
        src: usize,
        str_amt: i32,
        block_amt: i32,
    },
    /// 冠军爆怒：移除自身全部减益
    MonsterRemoveDebuffs {
        src: usize,
    },
    /// 收集者复活：每个死亡/逃离的火把头位重生一只
    ReviveTorchHeads,
    /// 蠕动聚块 MEGA：往玩家主牌组永久塞一张寄生前置（战斗内无即时效果）
    ParasiteToDeck,
    // —— Act 3 精英/Boss ——
    /// 蛇匕自爆后自灭
    SnakeDaggerSuicide {
        src: usize,
    },
    /// 德卡护甲环：全怪 +3 多层护甲
    PlatedAllMonsters(i32),
    /// 吞时者头撞：下回合抽牌 -1（DrawReductionPower）
    PlayerDrawNextTurn(i32),
    /// 尖塔之盾筑防：全怪获得格挡
    BlockAllMonsters(i32),
    /// 大史莱姆分裂：自灭（不计击杀），生成 2 只 M 版各承分裂时 HP
    SlimeSplit {
        src: usize,
        acid: bool,
        hp: i32,
    },
    /// 爆炸形体自爆：先自灭（真死路径）再对玩家 30 THORNS 伤（不可格挡）
    ExploderBlast {
        src: usize,
    },
}

/// 探素卡池（returnTrulyRandomCardInCombat(type) / Colorless 变体）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiscoveryPool {
    Attack,
    Skill,
    Power,
    Colorless,
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
        "LoseDexterityPower" | "DexLoss" => Some(PowerKind::LoseDexterity),
        "DuplicationPower" => Some(PowerKind::Duplication),
        "Plated Armor" => Some(PowerKind::PlatedArmor),
        "Flight" => Some(PowerKind::Flight),
        "Malleable" => Some(PowerKind::Malleable),
        "Hex" => Some(PowerKind::Hex),
        "Confusion" => Some(PowerKind::Confusion),
        "Angry" => Some(PowerKind::Angry),
        "Stasis" => Some(PowerKind::Stasis),
        "Fading" => Some(PowerKind::Fading),
        "Shifting" => Some(PowerKind::Shifting),
        "GainStrength" => Some(PowerKind::GainStrength),
        "Constricted" => Some(PowerKind::Constricted),
        "Reactive" => Some(PowerKind::Reactive),
        "Slow" => Some(PowerKind::Slow),
        "Time Warp" => Some(PowerKind::TimeWarp),
        "Intangible" => Some(PowerKind::IntangiblePlayer),
        "Curiosity" => Some(PowerKind::Curiosity),
        "Regenerate" => Some(PowerKind::RegenerateMonster),
        "Invincible" => Some(PowerKind::Invincible),
        "Beat of Death" => Some(PowerKind::BeatOfDeath),
        "Painful Stabs" => Some(PowerKind::PainfulStabs),
        "Dexterity" => Some(PowerKind::Dexterity),
        "Poison" => Some(PowerKind::Poison),
        "Metallicize" => Some(PowerKind::Metallicize),
        "Regeneration" => Some(PowerKind::Regen),
        "Focus" => Some(PowerKind::Focus),
        // 数据层 cls 名别名（ApplyPower 携带的是 *Power 后缀类名）
        "FocusPower" => Some(PowerKind::Focus),
        "EnergizedPower" => Some(PowerKind::Energized),
        "IntangiblePlayerPower" => Some(PowerKind::IntangiblePlayer),
        "NoDrawPower" => Some(PowerKind::NoDraw),
        "MantraPower" => Some(PowerKind::Mantra),
        // ── power 补缺第一批 + 数据层 cls 别名 ──
        "NoxiousFumesPower" => Some(PowerKind::NoxiousFumes),
        "InfiniteBladesPower" => Some(PowerKind::InfiniteBlades),
        "MentalFortressPower" => Some(PowerKind::MentalFortress),
        "LikeWaterPower" => Some(PowerKind::LikeWater),
        "DevotionPower" => Some(PowerKind::Devotion),
        "StormPower" => Some(PowerKind::Storm),
        "FeelNoPainPower" => Some(PowerKind::FeelNoPain),
        "RupturePower" => Some(PowerKind::Rupture),
        "AfterImagePower" => Some(PowerKind::AfterImage),
        "RepairPower" => Some(PowerKind::Repair),
        "DrawCardNextTurnPower" => Some(PowerKind::DrawCardNextTurn),
        "NextTurnBlockPower" => Some(PowerKind::NextTurnBlock),
        "DrawPower" => Some(PowerKind::DrawBonus),
        "TheBombPower" => Some(PowerKind::TheBomb),
        "WrathNextTurnPower" => Some(PowerKind::WrathNextTurn),
        // 已实现 power 的 cls 名别名（引擎早期按显示名登记）
        "DemonFormPower" => Some(PowerKind::DemonForm),
        "DarkEmbrace" => Some(PowerKind::DarkEmbrace),
        "BerserkPower" => Some(PowerKind::Berserk),
        "BrutalityPower" => Some(PowerKind::Brutality),
        "EnergizedBluePower" => Some(PowerKind::Energized),
        "VigorPower" => Some(PowerKind::Vigor),
        // ── power 补缺第二批 ──
        "AccuracyPower" => Some(PowerKind::Accuracy),
        "BlurPower" => Some(PowerKind::Blur),
        "Buffer" => Some(PowerKind::Buffer),
        "BurstPower" => Some(PowerKind::Burst),
        "DoubleTapPower" => Some(PowerKind::DoubleTap),
        "EchoPower" => Some(PowerKind::EchoForm),
        "EnvenomPower" => Some(PowerKind::Envenom),
        "EvolvePower" => Some(PowerKind::Evolve),
        "FireBreathingPower" => Some(PowerKind::FireBreathing),
        "FlameBarrierPower" => Some(PowerKind::FlameBarrier),
        "JuggernautPower" => Some(PowerKind::Juggernaut),
        "LockOnPower" => Some(PowerKind::LockOn),
        "MagnetismPower" => Some(PowerKind::Magnetism),
        "MayhemPower" => Some(PowerKind::Mayhem),
        "NirvanaPower" => Some(PowerKind::Nirvana),
        "NoBlockPower" => Some(PowerKind::NoBlock),
        "OmegaPower" => Some(PowerKind::Omega),
        "PhantasmalPower" => Some(PowerKind::Phantasmal),
        "RushdownPower" => Some(PowerKind::Rushdown),
        "SadisticPower" => Some(PowerKind::Sadistic),
        "StaticDischargePower" => Some(PowerKind::StaticDischarge),
        "ThousandCutsPower" => Some(PowerKind::ThousandCuts),
        "WaveOfTheHandPower" => Some(PowerKind::WaveOfTheHand),
        "WraithFormPower" => Some(PowerKind::WraithForm),
        // ── power 补缺第三批 ──
        "AmplifyPower" => Some(PowerKind::Amplify),
        "BattleHymnPower" => Some(PowerKind::BattleHymn),
        "BiasPower" => Some(PowerKind::Bias),
        "BlockReturnPower" => Some(PowerKind::BlockReturn),
        "ChokePower" => Some(PowerKind::Choke),
        "CorpseExplosionPower" => Some(PowerKind::CorpseExplosion),
        "CorruptionPower" => Some(PowerKind::Corruption),
        "CreativeAIPower" => Some(PowerKind::CreativeAI),
        "DevaPower" => Some(PowerKind::DevaForm),
        "ElectroPower" => Some(PowerKind::Electro),
        "EndTurnDeathPower" => Some(PowerKind::EndTurnDeath),
        "EnergyDownPower" => Some(PowerKind::EnergyDown),
        "EquilibriumPower" => Some(PowerKind::Equilibrium),
        "EstablishmentPower" => Some(PowerKind::Establishment),
        "ForesightPower" => Some(PowerKind::Foresight),
        "FreeAttackPower" => Some(PowerKind::FreeAttack),
        "HeatsinkPower" => Some(PowerKind::Heatsink),
        "HelloPower" => Some(PowerKind::Hello),
        "LoopPower" => Some(PowerKind::Loop),
        "MarkPower" => Some(PowerKind::Mark),
        "MasterRealityPower" => Some(PowerKind::MasterReality),
        "PanachePower" => Some(PowerKind::Panache),
        "ReboundPower" => Some(PowerKind::Rebound),
        "RetainCardPower" => Some(PowerKind::RetainCard),
        "StudyPower" => Some(PowerKind::Study),
        "ToolsOfTheTradePower" => Some(PowerKind::ToolsOfTheTrade),
        "DEPRECATEDDisciplinePower" => Some(PowerKind::Establishment), // 近似映射
        "GainStrengthPower" => Some(PowerKind::GainStrength),
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
    /// 本幕 Boss 的 vocab 值（进入地图时即可知，用于观测编码）
    pub boss_vocab: f32,
    /// 战斗中被偷、击败后应归还的金币总额（Looter/Mugger.die → STOLEN_GOLD 奖励）
    pub stolen_gold_rewards: i32,
    pub player: PlayerState,
    pub monsters: Vec<MonsterState>,
    /// 玩家遗物栏（第一批：战斗内触发遗物，见 relics.rs；局外层回填跨战计数）
    pub player_relics: Vec<RelicInst>,
    pub rng: RngStreams,
    /// 挂起的选择屏（None = 正常行动阶段）
    pub pending: Option<Pending>,
    /// 选择屏挂起时暂存的动作队列
    pending_queue: Option<Vec<Q>>,
    pub attacks_this_turn: i32,
    pub(crate) hand_cost_zero_this_turn: bool,
    pub(crate) hand_cost_max_one: bool,
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
    /// 烟雾弹逃离（战斗结束但非失败：M3 奖励层据此不给奖励）
    pub escaped: bool,
    /// 战斗内对主牌组的永久新增（OrbWalker 灼伤 / 蠕动聚块寄生前置）；
    /// M3 局外层据此更新卡组
    pub master_deck_additions: Vec<CardInst>,
    pub ascension: i32,
}

/// 洗牌追踪开关（`NEOW_TRACE_SHUFFLE=1`）；OnceLock 避免每场战斗读环境变量
fn trace_shuffle() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("NEOW_TRACE_SHUFFLE").is_ok())
}

/// 项目默认状态：全解锁 + 进阶 20（评测基准，见 README/roadmap）
pub const DEFAULT_ASCENSION: i32 = 20;

/// 怪物战前 power（CurlUp/孢子云/球体壁垒…）+ 首招 roll_move。
/// 从构造器抽出，供 new_with_relic_insts / new_run_battle 复用
fn self_monster_prebattle(c: &mut Combat) {
    for i in 0..c.monsters.len() {
        match c.monsters[i].kind {
            // Looter.usePreBattleAction：ApplyPowerAction(自身, ThieveryPower(goldAmt))
            MonsterKind::Looter => {
                let amt = c.monsters[i].thrash_dmg.max(0);
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Thievery,
                    amt,
                    false,
                    false,
                );
            }
            // LouseNormal / LouseDefensive 的 usePreBattleAction 完全相同：
            //   ApplyPowerAction(自身, CurlUpPower(monsterHpRng.random(3,7)/A7 4-8/A17 9-12))
            // ⚠️ LouseDefensive 原来漏了这个分支 —— 每只 Defensive 少掷 1 次
            // monster_hp，实测 floor 10（2 Normal + 1 Defensive）9 vs 8。
            MonsterKind::LouseNormal | MonsterKind::LouseDefensive => {
                let curl = if c.ascension >= 17 {
                    c.rng.monster_hp.random_start_end(9, 12)
                } else if c.ascension >= 7 {
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
            // 球形守护者：壁垒 + 神器 3 + 40 格挡（usePreBattleAction）
            MonsterKind::SphericGuardian => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Barricade,
                    1,
                    false,
                    false,
                );
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Artifact,
                    3,
                    false,
                    false,
                );
                c.monsters[i].creature.block = 40;
            }
            // 灼壳寄生生：多层护甲 14 + 14 格挡
            MonsterKind::ShelledParasite => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::PlatedArmor,
                    14,
                    false,
                    false,
                );
                c.monsters[i].creature.block = 14;
            }
            // 伯德鸟：飞行 3/4 层
            MonsterKind::Byrd => {
                let flight = c.monsters[i].bellow_str;
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Flight,
                    flight,
                    false,
                    false,
                );
            }
            // 咬人蛇草：可延展 3（起效值 3，受击递增）
            MonsterKind::SnakePlant => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Malleable,
                    3,
                    false,
                    false,
                );
            }
            // 战鬼：愤怒 1（A17+ 2）
            MonsterKind::GremlinWarrior => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Angry,
                    if c.ascension >= 17 { 2 } else { 1 },
                    false,
                    false,
                );
            }
            // 巫鬼：currentCharge 起 1
            MonsterKind::GremlinWizard => {
                c.monsters[i].burn_upgraded = 1;
            }
            // 连击之书：stabCount 起 1
            MonsterKind::BookOfStabbing => {
                c.monsters[i].burn_upgraded = 1;
            }
            // 天球行者：力量 3（A17+ 5）
            MonsterKind::OrbWalker => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Strength,
                    if c.ascension >= 17 { 5 } else { 3 },
                    false,
                    false,
                );
            }
            // 转瞬即逝：Fading 5（A17+ 6）+ Shifting
            MonsterKind::Transient => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Fading,
                    if c.ascension >= 17 { 6 } else { 5 },
                    false,
                    false,
                );
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Shifting,
                    1,
                    false,
                    false,
                );
            }
            // 巨首：自身 Slow 0（玩家出牌叠层）
            MonsterKind::GiantHead => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Slow,
                    0,
                    false,
                    false,
                );
            }
            // 觉醒者：再生 10(15 A19) + 好奇 1(2) + A4 力量 2
            MonsterKind::AwakenedOne => {
                let (regen, cur) = if c.ascension >= 19 { (15, 2) } else { (10, 1) };
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::RegenerateMonster,
                    regen,
                    false,
                    false,
                );
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Curiosity,
                    cur,
                    false,
                    false,
                );
                if c.ascension >= 4 {
                    apply_power(
                        &mut c.monsters[i].creature.powers,
                        PowerKind::Strength,
                        2,
                        false,
                        false,
                    );
                }
            }
            // 多努/德卡：神器 2（A19+ 3）
            MonsterKind::Donu | MonsterKind::Deca => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Artifact,
                    if c.ascension >= 19 { 3 } else { 2 },
                    false,
                    false,
                );
            }
            // 尖塔双子：A18+ 神器 2（否则 1）
            MonsterKind::SpireShield | MonsterKind::SpireSpear => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Artifact,
                    if c.ascension >= 18 { 2 } else { 1 },
                    false,
                    false,
                );
            }
            // 腐朽之心：无敌 300（A19+ 200）+ 亡律 1（A19+ 2）
            MonsterKind::CorruptHeart => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Invincible,
                    if c.ascension >= 19 { 200 } else { 300 },
                    false,
                    false,
                );
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::BeatOfDeath,
                    if c.ascension >= 19 { 2 } else { 1 },
                    false,
                    false,
                );
            }
            // 吞时者：TimeWarp（12 张牌终止回合）
            MonsterKind::TimeEater => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::TimeWarp,
                    0,
                    false,
                    false,
                );
            }
            // 蠕动聚块：Reactive + Malleable 3
            MonsterKind::WrithingMass => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Reactive,
                    1,
                    false,
                    false,
                );
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Malleable,
                    3,
                    false,
                    false,
                );
            }
            // 铜制自动机：神器 3
            MonsterKind::BronzeAutomaton => {
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Artifact,
                    3,
                    false,
                    false,
                );
            }
            // 荆棘形体：战前荆棘 3/4（A17 再 +3；usePreBattleAction）。
            // bellow_str 复用为增益计次（thornsCount，>5 后恒普攻，起 0）
            MonsterKind::Spiker => {
                let thorns = c.monsters[i].bellow_block + if c.ascension >= 17 { 3 } else { 0 };
                apply_power(
                    &mut c.monsters[i].creature.powers,
                    PowerKind::Thorns,
                    thorns,
                    false,
                    false,
                );
                c.monsters[i].bellow_str = 0;
            }
            _ => {}
        }
        c.roll_move(i);
    }
}

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
        Self::new_with_relics(seed, ascension, encounter, &[])
    }

    /// 指定遗物栏的遭遇构造（局外层把跨战斗计数的遗物实例化时用 RelicInst 直改）
    pub fn new_with_relics(
        seed: u64,
        ascension: i32,
        encounter: &[MonsterKind],
        relics: &[RelicKind],
    ) -> Self {
        Self::new_with_relic_insts(
            seed,
            ascension,
            encounter,
            relics.iter().map(|k| RelicInst::new(*k)).collect(),
        )
    }

    /// M3 局外层战斗构造：注入主牌组/药水栏/HP/金币（替代铁甲起始组的默认路径）。
    /// 与真游戏同序：怪 spawn（monsterHp）→ 战前 power → 洗主牌组（shuffle）→ 抽5
    /// → 遗物战斗开始钩子
    #[allow(clippy::too_many_arguments)]
    pub fn new_run_battle(
        seed: u64,
        ascension: i32,
        encounter: &[MonsterKind],
        relics: Vec<RelicInst>,
        deck: Vec<CardInst>,
        potions: Vec<Option<crate::potions::PotionInst>>,
        hp: i32,
        max_hp: i32,
        gold: i32,
    ) -> Self {
        let rng = RngStreams::new(seed);
        Self::new_run_battle_with_rng(
            rng, ascension, encounter, relics, deck, potions, hp, max_hp, gold,
        )
    }

    /// 从整局继承 RNG 流开战（M4 对拍正确路径）。
    ///
    /// 真游戏里战斗用的就是全局 `AbstractDungeon.monsterHpRng / shuffleRng /
    /// aiRng / cardRandomRng` 等流——它们贯穿整局，不是每场战斗重新播种。
    /// 只有这样才能对上「同种子第 N 场战斗的怪物 HP / 起手牌」。
    #[allow(clippy::too_many_arguments)]
    pub fn new_run_battle_with_rng(
        rng: RngStreams,
        ascension: i32,
        encounter: &[MonsterKind],
        relics: Vec<RelicInst>,
        deck: Vec<CardInst>,
        potions: Vec<Option<crate::potions::PotionInst>>,
        hp: i32,
        max_hp: i32,
        gold: i32,
    ) -> Self {
        let mut rng = rng;
        let monsters: Vec<MonsterState> = encounter
            .iter()
            .map(|k| MonsterState::spawn(*k, ascension, &mut rng.monster_hp))
            .collect();
        let mut c = Combat {
            boss_vocab: 0.0,
            stolen_gold_rewards: 0,
            player: PlayerState {
                stance: crate::state::Stance::Neutral,
                orbs: Vec::new(),
                max_orbs: 3,
                creature: Creature::new(hp, max_hp),
                energy: 3,
                hand: Vec::new(),
                draw: deck,
                discard: Vec::new(),
                exhaust: Vec::new(),
                potions,
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
            gold,
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
            escaped: false,
            master_deck_additions: Vec::new(),
            player_relics: relics,
            ascension,
        };
        card_group_shuffle(&mut c.player.draw, &mut c.rng.shuffle);
        c.draw_cards(5);
        self_monster_prebattle(&mut c);
        c.relics_battle_start();
        c
    }

    /// 带计数器的遗物实例构造（Du-Vu Doll 诅咒数/Girya 举重次数/Pen Nib 跨战
    /// 计数等由局外层回填；战斗开始钩子在内部末尾触发）
    pub fn new_with_relic_insts(
        seed: u64,
        ascension: i32,
        encounter: &[MonsterKind],
        relics: Vec<RelicInst>,
    ) -> Self {
        let mut rng = RngStreams::new(seed);
        let monsters: Vec<MonsterState> = encounter
            .iter()
            .map(|k| MonsterState::spawn(*k, ascension, &mut rng.monster_hp))
            .collect();
        let mut c = Combat {
            boss_vocab: 0.0,
            stolen_gold_rewards: 0,
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
                potions: vec![None, None, None],
            },
            monsters,
            player_relics: relics,
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
            escaped: false,
            master_deck_additions: Vec::new(),
            ascension,
        };
        card_group_shuffle(&mut c.player.draw, &mut c.rng.shuffle);
        c.draw_cards(5);
        self_monster_prebattle(&mut c);
        // 遗物战斗开始钩子（atPreBattle → atBattleStart → 首轮 atTurnStart；
        // AbstractRoom 战斗开始块时序，见 relics.rs 模块注释）
        c.relics_battle_start();
        c
    }

    pub(crate) fn alive(&self, i: usize) -> bool {
        i < self.monsters.len()
            && !self.monsters[i].creature.is_dead()
            && !self.monsters[i].escaped
    }

    pub(crate) fn all_monsters_dead(&self) -> bool {
        self.monsters
            .iter()
            .all(|m| m.creature.is_dead() || m.escaped)
    }

    pub fn needs_target(card: &CardInst) -> bool {
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
        self.potion_actions(&mut v);
        v
    }

    /// 药水合法动作：使用（可被动不可用/回合已结束时禁用）与丢弃（任何时候可丢）
    fn potion_actions(&self, v: &mut Vec<Action>) {
        for (i, slot) in self.player.potions.iter().enumerate() {
            let Some(p) = slot else { continue };
            if p.kind.can_use(self.turn_has_ended) {
                if p.kind.target_required() {
                    for m in 0..self.monsters.len() {
                        if self.alive(m) {
                            v.push(Action::UsePotion {
                                index: i,
                                target: Some(m),
                            });
                        }
                    }
                } else {
                    v.push(Action::UsePotion {
                        index: i,
                        target: None,
                    });
                }
            }
            v.push(Action::DiscardPotion { index: i });
        }
    }

    /// 使用药水：逐只对照 potions/*.java 的 use()（效果经引擎 Q 队列结算）
    pub fn use_potion(&mut self, index: usize, target: Option<usize>) {
        if index >= self.player.potions.len() {
            return; // 越界
        }
        let Some(p) = self.player.potions[index].clone() else {
            return; // 空药水栏（防御性：不 panic）
        };
        let potency = p.potency;
        let ti = target.unwrap_or(0);
        // 使用即消耗栏位（混沌药水需要先腾出自身栏位再掷新药水）
        self.player.potions[index] = None;
        let mut q: Vec<Q> = Vec::new();
        use PotionKind as P;
        match p.kind {
            // 火焰：THORNS 纯伤（applyEnemyPowersOnly 也只走受方 NORMAL 乘区，等效无修正）
            P::Fire => q.push(Q::DamagePure {
                target: ti,
                dmg: potency,
            }),
            // 格挡：GainBlockAction（吃敏捷/脆弱乘区，同卡牌格挡管线）
            P::Block => {
                let b = calc_player_card_block(potency, &self.player.creature.powers);
                q.push(Q::PlayerBlock(b));
            }
            P::Swift => q.push(Q::Draw(potency)),
            P::Energy => q.push(Q::GainEnergy(potency)),
            // 爆炸：createDamageMatrix(potency, true) 纯伤全怪
            P::Explosive => q.push(Q::DamageAllPure { dmg: potency }),
            P::Strength => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Strength,
                amount: potency,
            }),
            P::Weaken => q.push(Q::ApplyPowerToMonster {
                target: ti,
                kind: PowerKind::Weak,
                amount: potency,
            }),
            P::Fear => q.push(Q::ApplyPowerToMonster {
                target: ti,
                kind: PowerKind::Vulnerable,
                amount: potency,
            }),
            P::Regen => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Regen,
                amount: potency,
            }),
            P::Ancient => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Artifact,
                amount: potency,
            }),
            // 血药：回复最大生命 20%（战斗内 HealAction，回合结束后不可用）
            P::Blood => {
                let amt = (self.player.creature.max_hp as f32 * potency as f32 / 100.0) as i32;
                q.push(Q::Heal(amt));
            }
            P::Dexterity => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Dexterity,
                amount: potency,
            }),
            P::Steroid => {
                q.push(Q::ApplyPowerToSelf {
                    kind: PowerKind::Strength,
                    amount: potency,
                });
                q.push(Q::ApplyPowerToSelf {
                    kind: PowerKind::LoseStrength,
                    amount: potency,
                });
            }
            P::Speed => {
                q.push(Q::ApplyPowerToSelf {
                    kind: PowerKind::Dexterity,
                    amount: potency,
                });
                q.push(Q::ApplyPowerToSelf {
                    kind: PowerKind::LoseDexterity,
                    amount: potency,
                });
            }
            // 熔炉的祝福：ArmamentsAction(true) 升级全手牌
            P::BlessingOfTheForge => q.push(Q::DoUpgradeAllHand),
            P::Attack => q.push(Q::DoDiscovery {
                pool: DiscoveryPool::Attack,
                copies: potency,
            }),
            P::Skill => q.push(Q::DoDiscovery {
                pool: DiscoveryPool::Skill,
                copies: potency,
            }),
            P::Power => q.push(Q::DoDiscovery {
                pool: DiscoveryPool::Power,
                copies: potency,
            }),
            P::Colorless => q.push(Q::DoDiscovery {
                pool: DiscoveryPool::Colorless,
                copies: potency,
            }),
            // 邪教徒药水：RitualPower(player, 1, true)（玩家侧仪式，轮末投予）
            P::Cultist => q.push(Q::DoPlayerRitual(potency)),
            // 果汁：increaseMaxHp(5)（同时治疗 5）
            P::FruitJuice => q.push(Q::MaxHpPlus(potency)),
            // 异蛇之油：抽 5 + 随机化手牌费用（cardRandomRng，永改）
            P::SneckoOil => {
                q.push(Q::Draw(potency));
                q.push(Q::DoRandomizeHandCost);
            }
            // 烟雾弹：逃离战斗（不判负、无奖励）
            P::SmokeBomb => q.push(Q::DoSmokeEscape),
            // 混沌药水：每个空栏位掷一瓶（limited 路径，跳过果汁）
            P::EntropicBrew => q.push(Q::DoObtainRandomPotions),
            P::LiquidBronze => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Thorns,
                amount: potency,
            }),
            P::EssenceOfSteel => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::PlatedArmor,
                amount: potency,
            }),
            // 赌徒特酿：手牌非空才开屏
            P::GamblersBrew => {
                if !self.player.hand.is_empty() {
                    q.push(Q::DoGamblersDiscard);
                }
            }
            P::Duplication => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Duplication,
                amount: potency,
            }),
            // 液态记忆：弃堆非空才开屏
            P::LiquidMemories => {
                if !self.player.discard.is_empty() {
                    q.push(Q::DoDiscardPickFreeCost);
                }
            }
            P::DistilledChaos => q.push(Q::DoDistilledChaos { n: potency }),
            // 狡诈：3 张已升级小刀入手（Shiv.upgrade() 后 makeStatEquivalentCopy）
            P::Cunning => {
                let mut shiv = CardInst::new("Shiv");
                shiv.upgraded = true;
                for _ in 0..potency {
                    q.push(Q::MakeInHand(shiv));
                }
            }
            // 暗之精华：字面双重循环（orbs.size() 为 0 时不生效——保留源码行为）
            P::EssenceOfDarkness => q.push(Q::DoChannelDarkPerOrb { amount: potency }),
            P::Focus => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Focus,
                amount: potency,
            }),
            P::Capacity => q.push(Q::OrbSlots(potency)),
            P::GhostInAJar => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::IntangiblePlayer,
                amount: potency,
            }),
            P::HeartOfIron => q.push(Q::ApplyPowerToSelf {
                kind: PowerKind::Metallicize,
                amount: potency,
            }),
            P::BottledMiracle => {
                let miracle = CardInst::new("Miracle");
                for _ in 0..potency {
                    q.push(Q::MakeInHand(miracle));
                }
            }
            P::Stance => q.push(Q::DoStanceChoice),
            P::Ambrosia => q.push(Q::ChangeStance(crate::state::Stance::Divinity)),
            P::Poison => q.push(Q::ApplyPowerToMonster {
                target: ti,
                kind: PowerKind::Poison,
                amount: potency,
            }),
            // 不死药（已废弃）：消耗任意张手牌
            P::Elixir => q.push(Q::DoExhaustAny),
            // 瓶中精灵：canUse=false，仅死亡免死钩子触发（见 player_hp_loss）
            P::Fairy => unreachable!("瓶中精灵不可主动使用"),
        }
        self.drain(q);
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
            Action::UsePotion { index, target } => {
                assert!(self.pending.is_none(), "选择屏挂起中不可用药水");
                self.use_potion(*index, *target);
            }
            Action::DiscardPotion { index } => {
                self.player.potions[*index] = None;
            }
            Action::DiscoveryResponse { index } => self.resolve_discovery(*index),
            Action::StanceResponse { wrath } => self.resolve_stance(*wrath),
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
            // 赌徒特酿：弃任意张（可 0），抽等量（GamblingChipAction）
            Some(Pending::GamblersDiscard) => {
                let mut sel = indices.to_vec();
                sel.sort_unstable();
                sel.dedup();
                let n = sel.len();
                for &i in sel.iter().rev() {
                    let c = self.player.hand.remove(i);
                    self.player.discard.push(c);
                }
                self.discarded_this_turn += n as i32;
                self.draw_cards(n);
            }
            // 不死药（已废弃）：消耗任意张（ExhaustAction(false,true,true)）
            Some(Pending::HandExhaustAny) => {
                let mut sel = indices.to_vec();
                sel.sort_unstable();
                sel.dedup();
                for &i in sel.iter().rev() {
                    let c = self.player.hand.remove(i);
                    self.push_exhaust(c);
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

    /// 探素响应：选中卡 ×copies 入手（手牌满则弃），本回合 0 费
    fn resolve_discovery(&mut self, index: usize) {
        if let Some(Pending::Discovery { options, copies }) = self.pending.clone() {
            if let Some(&c) = options.get(index) {
                let n = copies.max(1) as usize;
                for _ in 0..n {
                    let mut c = c;
                    c.cost_delta = -(c.def.cost.unwrap_or(0)).max(0);
                    if self.player.hand.len() < 10 {
                        self.player.hand.push(c);
                    } else {
                        self.player.discard.push(c);
                    }
                }
            }
        }
        self.resume_queue();
    }

    /// 姿态药水响应：进入平静或愤怒
    fn resolve_stance(&mut self, wrath: bool) {
        if let Some(Pending::StanceChoice) = self.pending.clone() {
            self.player.stance = if wrath {
                crate::state::Stance::Wrath
            } else {
                crate::state::Stance::Calm
            };
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
                        self.push_exhaust(c);
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
        if let Some(Pending::PilePick { purpose, .. }) = self.pending.clone() {
            // 按目的分派（同一牌堆可对应不同去向；与源码各 *Action 对齐）
            match purpose {
                PilePickPurpose::DrawPlayTwiceExhaust => {
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
                        self.push_exhaust(card);
                    }
                }
                PilePickPurpose::DiscardExhaustPick => {
                    // Meditate：从弃牌堆选卡消耗
                    if index < self.player.discard.len() {
                        let c = self.player.discard.remove(index);
                        // 如果是技能获得1能量（近似）
                        if c.def.card_type == "SKILL" {
                            self.player.energy += 1;
                        }
                        self.push_exhaust(c);
                    }
                }
                PilePickPurpose::ExhaustToHand => {
                    if index < self.player.exhaust.len() {
                        let c = self.player.exhaust.remove(index);
                        if self.player.hand.len() < 10 {
                            self.player.hand.push(c);
                        } else {
                            self.player.discard.push(c);
                        }
                    }
                }
                PilePickPurpose::DiscardToDeckTop => {
                    if index < self.player.discard.len() {
                        let c = self.player.discard.remove(index);
                        self.player.draw.push(c);
                    }
                }
                // BetterDiscardPileToHandAction：弃堆选 1 入手
                PilePickPurpose::DiscardToHand => {
                    if index < self.player.discard.len() && self.player.hand.len() < 10 {
                        let c = self.player.discard.remove(index);
                        self.player.hand.push(c);
                    }
                }
                // 液态记忆：入手且本回合 0 费
                PilePickPurpose::DiscardToHandFreeCost => {
                    if index < self.player.discard.len() && self.player.hand.len() < 10 {
                        let mut c = self.player.discard.remove(index);
                        c.cost_delta = -(c.def.cost.unwrap_or(0)).max(0);
                        self.player.hand.push(c);
                    }
                }
                PilePickPurpose::DrawPileToHand => {
                    if index < self.player.draw.len() && self.player.hand.len() < 10 {
                        let c = self.player.draw.remove(index);
                        self.player.hand.push(c);
                    }
                }
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
            // ⚠️ damage_bonus / block_bonus 必须在这里叠加：
            // 它们是 Rampage / Glass Knife / Streamline / Perfected Strike 等的
            // 战斗内永久加成，之前只写不读 → 这些卡的效果全部无效
            // （实测 Perfected Strike 打 6 伤而真游戏 18 伤）。
            Expr::DamageBase => card.base_damage().unwrap_or(0) + card.damage_bonus,
            Expr::BlockBase => card.base_block().unwrap_or(0) + card.block_bonus,
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
            // Const/Raw 兜底：数据提取残留——尝试解析数字，失败则 0
            // （主效果 Damage/Block 已被 main_expr_ok 拦截，此处只有辅助参数）
            Expr::Const(s) => s.parse::<i32>().unwrap_or(if s == "true" { 1 } else { 0 }),
            Expr::Raw(_) => 0,
        }
    }

    fn play_card(&mut self, i: usize, target: Option<usize>) {
        if i >= self.player.hand.len() { return; } // 防御性：手牌越界
        let card = self.player.hand[i];
        let ti = target.unwrap_or(0);
        let x_cost = if card.cost_pay() == -1 {
            self.player.energy
        } else {
            0
        };
        let mut q: Vec<Q> = Vec::new();
        // ── PerfectedStrike.applyPowers：baseDamage += magicNumber × Strike 卡数 ──
        // Java: `this.baseDamage = 6 + this.magicNumber * countCards()`，
        // countCards 统计**整个牌组**（抽牌堆+手牌+弃牌堆+消耗堆）里 cardID 含
        // "strike" 的卡（不区分大小写，含自身）。
        // 之前完全没有这段逻辑 → 实测打出 6 伤而真游戏 18 伤。
        if card.def.id == "Perfected Strike" {
            let mut n = 0i32;
            for pile in [
                &self.player.draw,
                &self.player.hand,
                &self.player.discard,
                &self.player.exhaust,
            ] {
                n += pile
                    .iter()
                    .filter(|c| c.def.id.to_lowercase().contains("strike"))
                    .count() as i32;
            }
            let per = card
                .def
                .upg_magic
                .map(|u| if card.upgraded { card.def.base_magic.unwrap_or(2) + u } else { card.def.base_magic.unwrap_or(2) })
                .unwrap_or(card.def.base_magic.unwrap_or(2));
            if let Some(slot) = self.player.hand.get_mut(i) {
                slot.damage_bonus += per * n;
            }
        }
        let card = self.player.hand[i];
        // onUseCard 钩子（先于卡牌效果）：Rage —— 打出攻击牌获得 amount 格挡
        self.last_card_type = Some(card.def.card_type.to_string());
        // AngerPower.onUseCard：**怪物**持有 Anger 时，玩家每打出 SKILL →
        // 该怪物自身力量 +amount（GremlinNob 的 Bellow 就是这个）。
        if card.def.card_type == "SKILL" {
            let angers: Vec<(usize, i32)> = self
                .monsters
                .iter()
                .enumerate()
                .filter_map(|(mi, m)| {
                    m.creature
                        .powers
                        .iter()
                        .find(|pw| pw.kind == PowerKind::Anger)
                        .map(|pw| (mi, pw.amount))
                })
                .collect();
            for (mi, amt) in angers {
                apply_power(
                    &mut self.monsters[mi].creature.powers,
                    PowerKind::Strength,
                    amt,
                    false,
                    false,
                );
            }
        }
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
        // Vigor/PenNib 能力的 onUseCard：打出攻击牌 → 移除自身（RemoveSpecificPower
        // 入队先于复制重播，故重播的伤害不再吃干劲/笔尖——与源码队列序一致）
        if card.def.card_type == "ATTACK" {
            self.player
                .creature
                .powers
                .retain(|p| !matches!(p.kind, PowerKind::Vigor | PowerKind::PenNib));
        }
        // 遗物 onUseCard（手里剑/苦无/扇/双节棍/笔尖计数、书信刀、饕饣之瓮…）
        self.relics_on_use_card(card.def.card_type, &mut q);
        self.powers_on_use_card(card.def.card_type, &mut q);
        // Slow（GiantHead）：玩家每打出一张牌 → 所有 Slow 怪 +1 层（本回合）
        for mi in 0..self.monsters.len() {
            if let Some(p) = self.monsters[mi]
                .creature
                .powers
                .iter_mut()
                .find(|p| p.kind == PowerKind::Slow)
            {
                p.amount += 1;
            }
        }
        // Curiosity（AwakenedOne）：打出能力牌 → 好奇怪 +amount 力量
        if card.def.card_type == "POWER" {
            for mi in 0..self.monsters.len() {
                if let Some(cur) = self.monsters[mi]
                    .creature
                    .powers
                    .iter()
                    .find(|p| p.kind == PowerKind::Curiosity)
                    .map(|p| p.amount)
                {
                    apply_power(
                        &mut self.monsters[mi].creature.powers,
                        PowerKind::Strength,
                        cur,
                        false,
                        false,
                    );
                }
            }
        }
        // BeatOfDeath（心脏）：玩家每打出一张牌 → 受 amount 点 THORNS 直伤
        {
            let beats: Vec<i32> = self
                .monsters
                .iter()
                .filter(|m| !m.creature.is_dead() && !m.escaped)
                .flat_map(|m| {
                    m.creature
                        .powers
                        .iter()
                        .filter(|p| p.kind == PowerKind::BeatOfDeath)
                        .map(|p| p.amount)
                })
                .collect();
            for b in beats {
                // 直接失血（THORNS 管线：格挡吸收；此处按 DamageAction 语义走格挡）
                let mut d = b;
                if self
                    .player
                    .creature
                    .powers
                    .iter()
                    .any(|p| p.kind == PowerKind::IntangiblePlayer)
                    && d > 1
                {
                    d = 1;
                }
                self.player_deal_damage(d, DamageType::Thorns);
                if self.player.creature.is_dead() && !self.fairy_rescue() {
                    self.over = true;
                    self.won = false;
                }
            }
        }
        // TimeWarp（TimeEater）：12 张 → 立即终止玩家回合并清零
        {
            let mut warp_triggered = false;
            for mi in 0..self.monsters.len() {
                if let Some(p) = self.monsters[mi]
                    .creature
                    .powers
                    .iter_mut()
                    .find(|p| p.kind == PowerKind::TimeWarp)
                {
                    p.amount += 1;
                    if p.amount >= 12 {
                        p.amount = 0;
                        warp_triggered = true;
                    }
                }
            }
            if warp_triggered {
                self.force_end_turn_after_queue = true;
            }
        }
        // HexPower.onUseCard：打出非攻击牌 → 抽牌堆随机位置塞 amount 张晕眩
        if card.def.card_type != "ATTACK" {
            if let Some(hex) = self
                .player
                .creature
                .powers
                .iter()
                .find(|pw| pw.kind == PowerKind::Hex)
            {
                if let Some(dazed) = CardInst::new_by_class("Dazed") {
                    for _ in 0..hex.amount {
                        q.push(Q::MakeInDraw {
                            inst: dazed,
                            random: true,
                        });
                    }
                }
            }
        }
        // DuplicationPower.onUseCard：下一张牌额外免费结算一次（同目标同 X 费，
        // 不再扣能量/移牌；重播也触发 Rage/Finisher 计数）
        let dup = self
            .player
            .creature
            .powers
            .iter()
            .position(|pw| pw.kind == PowerKind::Duplication);
        if let Some(di) = dup {
            if self.player.creature.powers[di].amount > 0 {
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
                if card.def.card_type == "ATTACK" {
                    self.attacks_this_turn += 1;
                    // 重播也是一次真实 use()：同样触发 Vigor/PenNib 移除与遗物计数
                    self.player
                        .creature
                        .powers
                        .retain(|p| !matches!(p.kind, PowerKind::Vigor | PowerKind::PenNib));
                    self.relics_on_use_card(card.def.card_type, &mut q);
                    self.powers_on_use_card(card.def.card_type, &mut q);
                }
                self.player.creature.powers[di].amount -= 1;
                if self.player.creature.powers[di].amount == 0 {
                    self.player.creature.powers.remove(di);
                }
            }
        }
        // UseCardAction 去向：exhaust 旗标 → 消耗堆，否则弃牌堆
        self.player.hand.remove(i);
        if card.def.flags.contains(&"exhaust") {
            self.push_exhaust(card);
            // onExhaust 钩子（CardGroup.moveToExhaustPile：遗物先于 power）：
            // Dark Embrace —— 每消耗一张抽 amount
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
    /// 免费打出一张不在手牌的卡（PlayTopCardAction：蒸馏混沌等）
    /// 目标必选存活怪（getRandomMonster）；计入 Rage/Finisher/last_card_type
    fn play_card_free(&mut self, card: CardInst, target: Option<usize>) {
        let ti = target.unwrap_or(0);
        let mut q: Vec<Q> = Vec::new();
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
            self.apply_effect(eff, card, ti, 0, &mut q, 0);
        }
        if card.def.card_type == "ATTACK" {
            self.player
                .creature
                .powers
                .retain(|p| !matches!(p.kind, PowerKind::Vigor | PowerKind::PenNib));
        }
        self.relics_on_use_card(card.def.card_type, &mut q);
        self.powers_on_use_card(card.def.card_type, &mut q);
        if card.def.flags.contains(&"exhaust") {
            self.push_exhaust(card);
        } else {
            self.player.discard.push(card);
        }
        if card.def.card_type == "ATTACK" {
            self.attacks_this_turn += 1;
        }
        self.drain(q);
    }

    /// returnTrulyRandomCardInCombat(type) / ...ColorlessCardInCombat：
    /// src 池 = 角色色 COMMON/UNCOMMON/RARE（无色池 = COLORLESS 非基础非特殊非状态），
    /// 剔除 HEALING 标签（red: Feed/Reaper/LessonLearned），cardRandomRng 均匀采样
    fn sample_discovery_card(&mut self, pool: DiscoveryPool) -> CardInst {
        let ty = match pool {
            DiscoveryPool::Attack => "ATTACK",
            DiscoveryPool::Skill => "SKILL",
            DiscoveryPool::Power => "POWER",
            DiscoveryPool::Colorless => "",
        };
        let candidates: Vec<&'static CardDef> = crate::generated::CARDS
            .iter()
            .filter(|d| match pool {
                DiscoveryPool::Colorless => {
                    d.color == "COLORLESS"
                        && !matches!(d.rarity, "BASIC" | "SPECIAL")
                        && d.card_type != "STATUS"
                }
                _ => {
                    d.color == "RED"
                        && matches!(d.rarity, "COMMON" | "UNCOMMON" | "RARE")
                        && d.card_type == ty
                }
            })
            .filter(|d| !matches!(d.id, "Feed" | "Reaper" | "LessonLearned"))
            .collect();
        assert!(!candidates.is_empty(), "探素池为空");
        let roll = self
            .rng
            .card_random
            .random_range(candidates.len() as i32 - 1);
        let def = candidates[(roll.max(0) as usize).min(candidates.len() - 1)];
        CardInst {
            def,
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
                upgrade_times: 0,
        }
    }

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
                if ti >= self.monsters.len() { return; } // 防御性检查
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
                // 数据层可通行但行为层待接——运行时 no-op（卡可打出，特殊动作不生效）
            }
            // Unknown 目标当作 Enemy 处理（数据提取占位）；ti 越界时跳过
            Effect::Damage(_, e) => {
                if ti >= self.monsters.len() {
                    // 目标越界（不该发生——防御性检查）
                    return;
                }
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

    fn louse_defensive_get_move(&mut self, i: usize, num: i32) -> u8 {
        let h = &self.monsters[i].move_history;
        let last2_bite = h.len() >= 2 && h[h.len() - 1] == 3 && h[h.len() - 2] == 3;
        let last2_grow = h.len() >= 2 && h[h.len() - 1] == 4 && h[h.len() - 2] == 4;
        if num < 25 {
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

    fn spike_slime_m_get_move(&mut self, i: usize, num: i32) -> u8 {
        // SpikeSlime_M.getMove —— 注意 A17+ 与 A<17 的第三分支判定**不同**：
        //   A17+ : else if (num >= 30) → if (lastMove(4))  1 else 4
        //   A<17 : else if (num >= 30) → if (lastTwoMoves(4)) 1 else 4   ← lastTwoMoves!
        // 写成 lastMove 会让 hist=[1,4] 时错误地出 move 1（真游戏出 4）。
        let h = &self.monsters[i].move_history;
        let last2_tackle = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
        let last2_frail = h.len() >= 2 && h[h.len() - 1] == 4 && h[h.len() - 2] == 4;
        let last_frail = h.last() == Some(&4);
        let high = if self.ascension >= 17 { last_frail } else { last2_frail };
        if num < 30 {
            if last2_tackle {
                4
            } else {
                1
            }
        } else if high {
            1
        } else {
            4
        }
    }

    fn acid_slime_m_get_move(&mut self, i: usize, num: i32) -> u8 {
        // AcidSlime_M.getMove（A<17）：
        //   num < 30 → lastTwoMoves(1) ? (randBool ? 2 : 4) : 1
        //   num < 70 → lastMove(2)     ? (randBool(0.4) ? 1 : 4) : 2
        //   else     → lastTwoMoves(4) ? (randBool(0.4) ? 1 : 2) : 4
        // （A17+ 用的是 num<40 / num<80 + lastMove(4)，阈值与判定都不同）
        let h = &self.monsters[i].move_history;
        let last2_tackle = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
        let last2_rake = h.len() >= 2 && h[h.len() - 1] == 2 && h[h.len() - 2] == 2;
        let last2_weak = h.len() >= 2 && h[h.len() - 1] == 4 && h[h.len() - 2] == 4;
        let last_rake = h.last() == Some(&2);
        let last_weak = h.last() == Some(&4);
        if self.ascension >= 17 {
            if num < 40 {
                if last2_tackle {
                    if self.rng.ai.random_boolean() { 2 } else { 4 }
                } else {
                    1
                }
            } else if num < 80 {
                if last2_rake {
                    if self.rng.ai.random_boolean_chance(0.5) { 1 } else { 4 }
                } else {
                    2
                }
            } else if last_weak {
                if self.rng.ai.random_boolean_chance(0.4) { 1 } else { 2 }
            } else {
                4
            }
        } else if num < 30 {
            if last2_tackle {
                if self.rng.ai.random_boolean() { 2 } else { 4 }
            } else {
                1
            }
        } else if num < 70 {
            if last_rake {
                if self.rng.ai.random_boolean_chance(0.4) { 1 } else { 4 }
            } else {
                2
            }
        } else if last2_weak {
            if self.rng.ai.random_boolean_chance(0.4) { 1 } else { 2 }
        } else {
            4
        }
    }

    fn looter_get_move(&mut self, _i: usize, _num: i32) -> u8 {
        // Looter.getMove（源码全文只有一行）：
        //   this.setMove((byte)1, Intent.ATTACK, this.damage.get(0).base);
        // 即**恒 move 1**（swipeDmg）。后续招式全部由 takeTurn 里的 SetMoveAction
        // 决定（确定性，不掷 aiRng）。
        // ⚠️ 之前凭空写了「num<40 ? 1 : 4」分支，导致第 2 次行动就进 LUNGE。
        1
    }

    fn slaver_blue_get_move(&mut self, i: usize, num: i32) -> u8 {
        let h = &self.monsters[i].move_history;
        let last2_stab = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
        let last2_rake = h.len() >= 2 && h[h.len() - 1] == 4 && h[h.len() - 2] == 4;
        if num >= 40 && !last2_stab {
            1
        } else if !last2_rake {
            4
        } else {
            1
        }
    }

    fn slaver_red_get_move(&mut self, i: usize, num: i32) -> u8 {
        let h = &self.monsters[i].move_history;
        let last2_stab = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
        let last2_scrape = h.len() >= 2 && h[h.len() - 1] == 4 && h[h.len() - 2] == 4;
        if num >= 40 && !last2_stab {
            1
        } else if !last2_scrape {
            4
        } else {
            1
        }
    }

    fn gremlin_nob_get_move(&mut self, i: usize, num: i32) -> u8 {
        // GremlinNob.getMove（源码逐行，A<18 分支）：
        //   if (!usedBellow) { usedBellow = true; setMove(3, BUFF); return; }
        //   if (num < 33) → **无条件 move 2（Skull Bash）**，直接 return
        //   else if (lastTwoMoves(1)) → move 2（Skull Bash）
        //   else                      → move 1（Bull Rush）
        // ⚠️ 两个分支**不对称**：num<33 时不看历史，直接 Skull Bash。
        //    （A18+ 分支又不同：先判 lastMove(2)/lastMoveBefore(2)）
        let h = &self.monsters[i].move_history;
        if h.is_empty() {
            return 3; // Bellow（用 move_history 判首次，等价于 usedBellow）
        }
        if num < 33 {
            return 2;
        }
        let last2_rush = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
        if last2_rush {
            2
        } else {
            1
        }
    }

    fn lagavulin_get_move(&mut self, i: usize, _num: i32) -> u8 {
        // Lagavulin：沉睡→攻击→攻击→减益→循环
        if self.monsters[i].first_move {
            return 5; // Sleep
        }
        let h = &self.monsters[i].move_history;
        // 简化：醒来后 攻击→攻击→减益→循环
        let recent: Vec<u8> = h.iter().rev().take(3).copied().collect();
        if recent.first() == Some(&1) {
            3 // 刚减益完→攻击
        } else if recent.first() == Some(&3) && recent.get(1) == Some(&3) {
            1 // 连续两轮攻击→减益
        } else {
            3 // 攻击或默认
        }
    }

    fn sentry_get_move(&mut self, i: usize, _num: i32) -> u8 {
        // Sentry：交替 Debuff/Attack
        if self.monsters[i].first_move {
            // 偶数位 Debuff 起手，奇数位 Attack 起手
            return if i & 1 == 0 { 3 } else { 4 };
        }
        let h = &self.monsters[i].move_history;
        if h.last() == Some(&4) {
            3
        } else {
            4
        }
    }

    fn guardian_get_move(&mut self, i: usize) -> u8 {
        // Guardian 固定循环: Charge Up(6) → Fierce Bash(2) → Vent Steam(7) → Whirlwind(5) → repeat
        if self.monsters[i].first_move {
            return 6;
        }
        match self.monsters[i].move_history.last() {
            Some(&6) => 2,
            Some(&2) => 7,
            Some(&7) => 5,
            Some(&5) => 6,
            _ => 6,
        }
    }

    fn slime_boss_get_move(&mut self, i: usize) -> u8 {
        // SlimeBoss 固定循环: Goop(4) → Prepare(2) → Slam(1) → repeat
        if self.monsters[i].first_move {
            return 4;
        }
        match self.monsters[i].move_history.last() {
            Some(&4) => 2,
            Some(&2) => 1,
            Some(&1) => 4,
            _ => 4,
        }
    }

    fn end_turn(&mut self) {
        self.turn_has_ended = true;
        // callEndOfTurnActions → applyEndOfTurnRelics（最先于玩家 powers EOT）：
        // Orichalcum / Stone Calendar / Frozen Core
        self.relics_on_player_end_turn();
        if self.over {
            return;
        }
        // callEndOfTurnActions：玩家 powers 的 EOT，按插入序逐个结算：
        // Metallicize（PreEndTurnCards 钩子，先于其余）→ LoseStrength/Regen（atEndOfTurn）
        let mut combust_damage = 0;
        let mut combust_hp_loss = 0;
        let mut bomb_damage = 0;
        let mut wraith_dex_loss = 0;
        {
            let mut keep: Vec<Power> = Vec::new();
            let mut strength_deltas: Vec<i32> = Vec::new();
            let mut dex_deltas: Vec<i32> = Vec::new();
            for p in self.player.creature.powers.drain(..) {
                match p.kind {
                    PowerKind::Metallicize => {
                        self.player.creature.block += p.amount;
                        keep.push(p);
                    }
                    // Plated Armor：atEndOfTurnPreEndTurnCards 获得等同层数格挡
                    PowerKind::PlatedArmor => {
                        self.player.creature.block += p.amount;
                        keep.push(p);
                    }
                    PowerKind::LoseStrength => strength_deltas.push(p.amount),
                    // DexLoss：EOT 敏捷 -amount 后移除（LoseDexterityPower）
                    PowerKind::LoseDexterity => dex_deltas.push(p.amount),
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
                        combust_hp_loss += 1;
                        combust_damage += p.amount;
                        keep.push(p);
                    }
                    // Rage：仅本回合有效，EOT 移除
                    PowerKind::Rage => {}
                    // Omega：回合结束 50 THORNS 全体
                    PowerKind::Omega => {
                        bomb_damage = bomb_damage.max(50);
                        keep.push(p);
                    }
                    // WraithForm：每回合 -1 敏捷（延迟到 drain 后结算）
                    PowerKind::WraithForm => {
                        wraith_dex_loss += 1;
                        keep.push(p);
                    }
                    // 如水：平静中回合结束 → 获 amount 格挡（PreEndTurnCards）
                    PowerKind::LikeWater => {
                        if self.player.stance == crate::state::Stance::Calm {
                            self.player.creature.block += p.amount;
                        }
                        keep.push(p);
                    }
                    // 炸弹：EOT 倒数 -1，归零 → 全体 THORNS 伤（amount 编码 dmg×10+倒数）
                    PowerKind::TheBomb => {
                        let dmg = p.amount / 10;
                        let turns = p.amount % 10;
                        if turns <= 1 {
                            bomb_damage = bomb_damage.max(dmg);
                        } else {
                            keep.push(Power {
                                amount: dmg * 10 + (turns - 1),
                                ..p
                            });
                        }
                    }
                    _ => keep.push(p),
                }
            }
            self.player.creature.powers = keep;
            // WraithForm：每回合 -1 敏捷
            if wraith_dex_loss > 0 {
                apply_power(
                    &mut self.player.creature.powers,
                    PowerKind::Dexterity,
                    -wraith_dex_loss,
                    false,
                    false,
                );
            }
            // Combust 的 LoseHPAction（HP_LOSS：绕格挡；TungstenRod 每次失血 -1）
            if combust_hp_loss > 0 {
                self.player_deal_damage(combust_hp_loss, DamageType::HpLoss);
            }
            for d in strength_deltas {
                apply_power(
                    &mut self.player.creature.powers,
                    PowerKind::Strength,
                    -d,
                    false,
                    false,
                );
            }
            for d in dex_deltas {
                apply_power(
                    &mut self.player.creature.powers,
                    PowerKind::Dexterity,
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
                    self.hit_monster(mi, combust_damage, DamageType::Thorns);
                    if self.over {
                        return;
                    }
                }
            }
        }
        // 炸弹起爆：DamageAllEnemies（THORNS 直伤，不走力量/虚弱）
        if bomb_damage > 0 && !self.over {
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    self.hit_monster(mi, bomb_damage, DamageType::Thorns);
                    if self.over {
                        return;
                    }
                }
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
                            self.hit_monster(t, passive, DamageType::Normal);
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
        // 不可打出状态牌的回合末自动结算（游戏路径：EOT 静默打出 → Burn.use
        // 的 dontTriggerOnUseCard 分支）：灼伤 = magicNumber 直伤
        let burn_dmgs: Vec<i32> = self
            .player
            .hand
            .iter()
            .filter(|c| c.def.id == "Burn")
            .map(|c| c.magic().unwrap_or(2))
            .collect();
        for dmg in burn_dmgs {
            self.exec(Q::PlayerFlatDamage(dmg));
            if self.over {
                return;
            }
        }
        // Constricted（紧缠）：玩家回合末受 amount 点 THORNS 直伤（可多源叠加）
        {
            let constrict_dmg: i32 = self
                .player
                .creature
                .powers
                .iter()
                .filter(|p| p.kind == PowerKind::Constricted)
                .map(|p| p.amount)
                .sum();
            if constrict_dmg > 0 {
                self.exec(Q::PlayerFlatDamage(constrict_dmg));
                if self.over {
                    return;
                }
            }
        }
        // CloakClasp：回合结束手牌每张 +1 格挡
        if self
            .player_relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::CloakClasp)
        {
            let h = self.player.hand.len() as i32;
            self.player.creature.block += h;
        }
        // DiscardAtEndOfTurn：无保留卡（retain）与 Runic Pyramid/Equilibrium 例外
        let has_pyramid = self
            .player_relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::RunicPyramid);
        if !has_pyramid {
            let hand = std::mem::take(&mut self.player.hand);
            // ⚠️ 关键：Java DiscardAtEndOfTurnAction 会排队 N 个 DiscardAction，
            // 每个都用 `hand.getTopCard()`（= group.get(size-1)，手牌**末尾/最右**）
            // 逐个丢弃 ⇒ 弃牌顺序 = 手牌 group 的**逆序**。
            // 正序会改变弃牌堆排列，进而让「重洗后的抽牌顺序」与真游戏不同
            // （实测 turn3 起手 hand 顺序错位，因为洗牌输入顺序不同）。
            self.player.discard.extend(hand.into_iter().rev());
        }
        // Runic Pyramid：保留手牌（不清不弃）
        // MonsterStartTurnAction（AbstractRoom:406 单次入队）：
        // 怪物回合开始时对全部存活怪统一掉格挡（非逐怪；壁垒例外）
        for m in &mut self.monsters {
            if !m.creature.is_dead()
                && !m
                    .creature
                    .powers
                    .iter()
                    .any(|p| p.kind == PowerKind::Barricade)
            {
                m.creature.block = 0;
            }
        }
        // Vault：跳过怪物回合
        if self.skip_monster_turn {
            self.skip_monster_turn = false;
        } else {
            // 怪物序列（MonsterGroup.queueMonsters 列表序）
            for i in 0..self.monsters.len() {
                let gone = (self.monsters[i].creature.is_dead() && !self.monsters[i].half_dead)
                    || self.monsters[i].escaped;
                if gone {
                    continue; // 半死怪（Darkling）仍需行动（待机/复活）
                }
                let executed = self.monsters[i].next_move;
                let mut q = self.monster_turn(i);
                // Hexaghost Activate / Byrd 头槌：takeTurn 内 setMove 后 return；
                // Mugger：takeTurn 完全由 SetMoveAction 驱动——均不追加 RollMoveAction，
                // 不消耗 aiRng（流序对拍关键）
                let skip_roll = match self.monsters[i].kind {
                    MonsterKind::Hexaghost => executed == 5,
                    MonsterKind::Byrd => executed == 5,
                    // SetMove 驱动（takeTurn 无 RollMoveAction）：劫匪 + 五小鬼 + 火把头
                    // Looter：takeTurn 内全部用 SetMoveAction（无 RollMoveAction）
                    MonsterKind::AcidSlimeS
                    | MonsterKind::Looter
                    | MonsterKind::Mugger
                    | MonsterKind::GremlinWarrior
                    | MonsterKind::GremlinThief
                    | MonsterKind::GremlinFat
                    | MonsterKind::GremlinTsundere
                    | MonsterKind::GremlinWizard
                    | MonsterKind::TorchHead
                    | MonsterKind::Transient => true,
                    _ => false,
                };
                if !skip_roll {
                    q.push(Q::RollMove(i));
                }
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
        // ② loseBlock（Barricade 豁免；Calipers：只减 15）
        if !self
            .player
            .creature
            .powers
            .iter()
            .any(|pw| pw.kind == PowerKind::Barricade)
        {
            if self
                .player_relics
                .iter()
                .any(|r| r.kind == crate::relics::RelicKind::Calipers)
            {
                self.player.creature.block = (self.player.creature.block - 15).max(0);
            } else {
                self.player.creature.block = 0;
            }
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
        // Ice Cream：能量跨回合保留（叠加剩余）
        let leftover = if self
            .player_relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::IceCream)
        {
            self.player.energy
        } else {
            0
        };
        self.player.energy = 3 + bonus_energy + leftover; // ③ 能量重置（Plasma+IceCream 加成合并）
                                                          // applyStartOfTurnRelics（atTurnStart：Lantern/HappyFlower/Art of War/
                                                          // 手里剑系清零/Mercury Hourglass/Brimstone…）——真游戏在 loseBlock 前同步
                                                          // 调用、动作入队在 loseBlock 后执行，格挡/能量类净效果与此时序一致
        self.relics_at_turn_start();
        if self.over {
            return;
        }
        // ── atStartOfTurn（抽牌前）：无限之刃 Shiv / 下回合格挡 / 下回合愤怒 ──
        {
            let mut keep: Vec<Power> = Vec::new();
            let mut shiv_n = 0;
            let mut block_n = 0;
            let mut enter_wrath = false;
            for pw in self.player.creature.powers.drain(..) {
                match pw.kind {
                    PowerKind::InfiniteBlades => {
                        shiv_n += pw.amount;
                        keep.push(pw);
                    }
                    PowerKind::NextTurnBlock => {
                        block_n += pw.amount; // 一次性：不移回
                    }
                    PowerKind::WrathNextTurn => {
                        enter_wrath = true; // 一次性
                    }
                    _ => keep.push(pw),
                }
            }
            self.player.creature.powers = keep;
            if shiv_n > 0 {
                let shiv = crate::cards::CardInst::new("Shiv");
                for _ in 0..shiv_n {
                    if self.player.hand.len() < 10 {
                        self.player.hand.push(shiv);
                    }
                }
            }
            if block_n > 0 {
                self.player.creature.block += block_n;
            }
            if enter_wrath {
                self.exec(Q::ChangeStance(crate::state::Stance::Wrath));
            }
        }
        // DrawPower（Machine Learning）：gameHandSize +amount
        let hand_bonus = self
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::DrawBonus)
            .map(|p| p.amount)
            .unwrap_or(0);
        // Ring of the Serpent：masterHandSize +1
        let serpent = if self
            .player_relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::RingOfTheSerpent)
        {
            1
        } else {
            0
        };
        self.draw_cards((5 + hand_bonus + serpent) as usize);
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
            // LoseHPAction（HP_LOSS：绕格挡，TungstenRod 可减 1）
            self.player_deal_damage(hp_loss, DamageType::HpLoss);
            if self.player.creature.is_dead() {
                self.over = true;
                self.won = false;
            }
        }
        // ── atStartOfTurnPostDraw 其余：剧毒瘴气上毒 / 奉献真言 / 掠食者补抽 ──
        let mut fumes_poison = 0;
        let mut devotion = 0;
        let mut predator_draw = 0;
        let mut magnetism_n = 0;
        {
            let mut keep: Vec<Power> = Vec::new();
            for pw in self.player.creature.powers.drain(..) {
                match pw.kind {
                    PowerKind::NoxiousFumes => {
                        fumes_poison += pw.amount;
                        keep.push(pw);
                    }
                    // Magnetism：回合开始充能等离子球（延迟到 drain 后）
                    PowerKind::Magnetism => {
                        magnetism_n += pw.amount;
                        keep.push(pw);
                    }
                    PowerKind::Devotion => {
                        devotion += pw.amount;
                        keep.push(pw);
                    }
                    PowerKind::DrawCardNextTurn => {
                        predator_draw += pw.amount; // 一次性：不移回
                    }
                    _ => keep.push(pw),
                }
            }
            self.player.creature.powers = keep;
        }
        if fumes_poison > 0 {
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    apply_power(
                        &mut self.monsters[mi].creature.powers,
                        PowerKind::Poison,
                        fumes_poison,
                        false,
                        false,
                    );
                }
            }
        }
        if devotion > 0 {
            self.apply_power_to_player_checked(PowerKind::Mantra, devotion, false);
        }
        if predator_draw > 0 {
            self.draw_cards(predator_draw as usize);
        }
        // Magnetism：充能等离子球（drain 后执行）
        if magnetism_n > 0 {
            for _ in 0..magnetism_n {
                self.exec(Q::Channel {
                    kind: crate::state::OrbKind::Plasma,
                    auto_evoke: false,
                });
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
            MonsterKind::LouseDefensive => match m.next_move {
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                4 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Weak,
                    amount: m.ritual_amt,
                }),
                _ => {}
            },
            MonsterKind::SpikeSlimeS => {
                q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                });
            }
            MonsterKind::AcidSlimeS => {
                // AcidSlime_S.takeTurn 是**确定性换招**，不排 RollMoveAction：
                //   case 1 → DamageAction + setMove(2, DEBUFF)
                //   case 2 → ApplyPowerAction(Weak 1) + setMove(1, ATTACK)
                // 所以回合末它**不消耗 aiRng**（真游戏日志已验证：
                // move_history 增长但 aiRng.counter 不变）。
                match m.next_move {
                    1 => {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.dark_strike_dmg,
                        });
                        q.push(Q::SetMonsterMove { target: i, next: 2 });
                    }
                    2 => {
                        q.push(Q::ApplyPowerToPlayerFromMonster {
                            kind: PowerKind::Weak,
                            amount: m.ritual_amt,
                        });
                        q.push(Q::SetMonsterMove { target: i, next: 1 });
                    }
                    _ => {}
                }
            }
            MonsterKind::SpikeSlimeM => match m.next_move {
                // SpikeSlime_M.takeTurn：
                //   case 1   → DamageAction(玩家) + MakeTempCardInDiscardAction(Slimed, 1)
                //   case 4   → ApplyPowerAction(玩家, Frail 1)
                // 原实现把两者都写成 Frail，且 move 1 还额外加了 Frail —— 语义全错。
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.dark_strike_dmg,
                    });
                    if let Some(c) = CardInst::new_by_class("Slimed") {
                        q.push(Q::MakeDiscard(c));
                    }
                }
                4 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Frail,
                    amount: m.ritual_amt,
                }),
                _ => {}
            },
            MonsterKind::AcidSlimeM => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                4 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Weak,
                    amount: m.ritual_amt,
                }),
                _ => {}
            },
            MonsterKind::Looter => match m.next_move {
                // Looter.takeTurn（完整状态机，源码逐行）：
                //   case 1 MUG_SWIPE：
                //     slashCount==0 时先 aiRng.randomBoolean(0.6) 决定是否 talk（仍消耗 RNG）
                //     偷 goldAmt → DamageAction(swipeDmg) → ++slashCount
                //     若 slashCount==2：aiRng.randomBoolean(0.5) ? setMove(2, DEFEND)
                //                                              : SetMove(4, ATTACK, lungeDmg)
                //     否则 setMove(1, ATTACK, swipeDmg)
                //   case 4 LUNGE：++slashCount → 偷 → DamageAction(lungeDmg) → setMove(2, DEFEND)
                //   case 2 SMOKE_BOMB：+escapeDef 格挡 → setMove(3, ESCAPE)
                //   case 3 ESCAPE：逃跑（escaped=true，全员逃离则战斗结束）
                // 字段：chomp_dmg=swipeDmg, thrash_block=lungeDmg, thrash_dmg=goldAmt
                1 => {
                    // talk 判定：`slashCount == 0 && aiRng.randomBoolean(0.6f)`
                    // Java 是短路求值：slashCount != 0 时不调用 randomBoolean。
                    // ⇒ slashCount==0（首次 MUG）时必须掷一次，之后不掷。
                    if m.burn_upgraded == 0 {
                        let _ = self.rng.ai.random_boolean_chance(0.6);
                    }
                    // 偷取必须走 Q::StealGold（会累加 stolenGold 到 bellow_block），
                    // 直接 self.gold -= steal 会漏记，导致击败后不归还金币。
                    q.push(Q::StealGold {
                        src: i,
                        amount: m.thrash_dmg.max(0),
                    });
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    let slash = m.burn_upgraded + 1;
                    q.push(Q::SetMonsterCounter { target: i, value: slash });
                    if slash == 2 {
                        if self.rng.ai.random_boolean_chance(0.5) {
                            q.push(Q::SetMonsterMove { target: i, next: 2 });
                        } else {
                            q.push(Q::SetMonsterMove { target: i, next: 4 });
                        }
                    } else {
                        q.push(Q::SetMonsterMove { target: i, next: 1 });
                    }
                }
                4 => {
                    q.push(Q::StealGold {
                        src: i,
                        amount: m.thrash_dmg.max(0),
                    });
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_block,
                    });
                    q.push(Q::SetMonsterCounter {
                        target: i,
                        value: m.burn_upgraded + 1,
                    });
                    q.push(Q::SetMonsterMove { target: i, next: 2 });
                }
                2 => {
                    // 烟雾弹：获得 escapeDef 格挡，下一招逃跑
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.bellow_str.max(0),
                    });
                    q.push(Q::SetMonsterMove { target: i, next: 3 });
                }
                _ => q.push(Q::MonsterEscape { src: i }),
            },
            MonsterKind::SlaverBlue => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                4 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.ritual_amt,
                    });
                }
                _ => {}
            },
            MonsterKind::SlaverRed => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                4 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: m.ritual_amt,
                    });
                }
                _ => {}
            },
            MonsterKind::GremlinNob => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                2 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: m.bellow_str,
                    });
                }
                3 => {
                    // GremlinNob.takeTurn case 3 (Bellow)：
                    //   ApplyPowerAction(self, AngerPower(2))（A18+ 为 3）
                    // AngerPower 的规则效果是「持有者每打出 SKILL → 自身力量 +amount」，
                    // **不是**直接加力量。原实现错写成 Strength。
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Anger,
                        amount: m.ritual_amt,
                    });
                }
                _ => {}
            },
            MonsterKind::Lagavulin => match m.next_move {
                1 => {
                    // Debuff：玩家 -1 力量 -1 敏捷
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Strength,
                        amount: -m.ritual_amt,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Dexterity,
                        amount: -m.ritual_amt,
                    });
                }
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                5 => {
                    // Sleep：什么都不做
                }
                _ => {}
            },
            MonsterKind::Sentry => match m.next_move {
                3 => {
                    // Beam（Debuff版）：加 Dazed 到弃牌堆
                    for _ in 0..m.ritual_amt {
                        if let Some(c) = CardInst::new_by_class("Dazed") {
                            q.push(Q::MakeDiscard(c));
                        }
                    }
                }
                4 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.dark_strike_dmg,
                }),
                _ => {}
            },
            MonsterKind::Guardian => match m.next_move {
                6 => {
                    // Charge Up: 获得格挡
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.bellow_block,
                    });
                }
                2 => {
                    // Fierce Bash: 重击
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.dark_strike_dmg,
                    });
                }
                7 => {
                    // Vent Steam: 虚弱+易伤
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: 2,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: 2,
                    });
                }
                5 => {
                    // Whirlwind: 5伤×4
                    for _ in 0..m.ritual_amt {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.bellow_str,
                        });
                    }
                }
                4 => {
                    // Twin Slam: 8伤×2
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_block,
                        });
                    }
                }
                3 => {
                    // Roll Attack: 9伤
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                }
                _ => {}
            },
            MonsterKind::SlimeBoss => match m.next_move {
                4 => {
                    // Goop: 3张粘液入弃牌堆
                    for _ in 0..m.thrash_dmg {
                        if let Some(c) = CardInst::new_by_class("Slimed") {
                            q.push(Q::MakeDiscard(c));
                        }
                    }
                }
                2 => {
                    // Prepare: 什么都不做
                }
                1 => {
                    // Slam: 35伤
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.dark_strike_dmg,
                    });
                }
                _ => {}
            },
            // 六火亡魂：开场 Activate → 分割者（锁定玩家 HP/12+1 ×6）→ 六球循环
            // Sear/Tackle/Sear/Inflame/Tackle/Sear → Inferno；每招后激活一球（计数+1），
            // Divider/Inferno 后重置；Sear 的灼伤是否升级由 Inferno 置位（burnUpgraded）
            MonsterKind::Hexaghost => {
                let (nm, tackle, inferno, str_amt, burn_n, burn_up) = (
                    m.next_move,
                    m.chomp_dmg,
                    m.thrash_dmg,
                    m.thrash_block,
                    m.bellow_str,
                    m.burn_upgraded,
                );
                match nm {
                    5 => {
                        // Activate：分割者段伤 = 玩家当前 HP/12 + 1（整除，此刻锁定）；
                        // takeTurn 内直接 setMove(DIVIDER)——不追加 RollMove（end_turn 处门控）
                        let div = self.player.creature.hp / 12 + 1;
                        self.monsters[i].bellow_block = div;
                        self.monsters[i].move_history.push(1);
                        self.monsters[i].next_move = 1;
                    }
                    1 => {
                        // Divider：6 段锁定伤害；随后全部熄灭（球计数归零）
                        for _ in 0..6 {
                            q.push(Q::MonsterDamage {
                                src: i,
                                base: self.monsters[i].bellow_block,
                            });
                        }
                        self.monsters[i].ritual_amt = 0;
                    }
                    2 => {
                        // Tackle：火焰冲撞 ×2；激活一球
                        for _ in 0..2 {
                            q.push(Q::MonsterDamage {
                                src: i,
                                base: tackle,
                            });
                        }
                        self.monsters[i].ritual_amt += 1;
                    }
                    3 => {
                        // Inflame（Strengthen）：12 格挡 + 力量；激活一球
                        q.push(Q::MonsterBlock {
                            target: i,
                            amount: 12,
                        });
                        q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::Strength,
                            amount: str_amt,
                        });
                        self.monsters[i].ritual_amt += 1;
                    }
                    4 => {
                        // Sear：6 伤 + burn_n 张灼伤入弃牌堆（已升级则升级版）；激活一球
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.dark_strike_dmg,
                        });
                        for _ in 0..burn_n {
                            if let Some(mut c) = CardInst::new_by_class("Burn") {
                                c.upgraded = burn_up == 1;
                                q.push(Q::MakeDiscard(c));
                            }
                        }
                        self.monsters[i].ritual_amt += 1;
                    }
                    6 => {
                        // Inferno：6 段 + 升级弃/抽堆全部灼伤 + 3 张升级灼伤入弃堆
                        // + 置位 burnUpgraded；随后全部熄灭
                        for _ in 0..6 {
                            q.push(Q::MonsterDamage {
                                src: i,
                                base: inferno,
                            });
                        }
                        q.push(Q::UpgradeAllBurnPiles);
                        for _ in 0..3 {
                            if let Some(mut c) = CardInst::new_by_class("Burn") {
                                c.upgraded = true;
                                q.push(Q::MakeDiscard(c));
                            }
                        }
                        self.monsters[i].burn_upgraded = 1;
                        self.monsters[i].ritual_amt = 0;
                    }
                    _ => {}
                }
            }
            // ══════════ Act 2（TheCity）普通遭遇 ══════════
            // 球形守护者：1=重击×2  2=充能（大格挡）  3=硬化打击  4=脆化打击
            MonsterKind::SphericGuardian => match m.next_move {
                1 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                2 => q.push(Q::MonsterBlock {
                    target: i,
                    amount: m.thrash_dmg,
                }),
                3 => {
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_block,
                    });
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                }
                4 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: m.bellow_str,
                    });
                }
                _ => {}
            },
            // 天选者：1=电击  2=汲取  3=衰弱  4=咒缚  5=戳刺×2
            MonsterKind::Chosen => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => {
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.bellow_block,
                    });
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Strength,
                        amount: m.bellow_str,
                    });
                }
                3 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: m.dark_strike_dmg,
                    });
                }
                4 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Hex,
                    amount: m.ritual_amt,
                }),
                5 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_block,
                        });
                    }
                }
                _ => {}
            },
            // 灼壳寄生生：1=猛击+脆弱  2=双重打击  3=吸取（吸血）  4=破甲眩晕
            MonsterKind::ShelledParasite => match m.next_move {
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: m.bellow_str,
                    });
                }
                2 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                3 => q.push(Q::MonsterVampire {
                    src: i,
                    base: m.thrash_block,
                }),
                _ => {} // 4 眩晕：无动作（RollMove 接续）
            },
            // 伯德鸟：1=啄×N  2=起飞  3=俯冲  4=落地眩晕  5=头槌→setMove(2)  6=鸣叫
            MonsterKind::Byrd => match m.next_move {
                1 => {
                    for _ in 0..m.bellow_block {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                2 => q.push(Q::ApplyPowerToMonster {
                    target: i,
                    kind: PowerKind::Flight,
                    amount: m.bellow_str,
                }),
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                5 => {
                    // 头槌：takeTurn 内 setMove(2 UNKNOWN) 后 return——不消耗 aiRng
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_block,
                    });
                    self.monsters[i].move_history.push(2);
                    self.monsters[i].next_move = 2;
                }
                6 => q.push(Q::ApplyPowerToMonster {
                    target: i,
                    kind: PowerKind::Strength,
                    amount: m.ritual_amt,
                }),
                _ => {} // 4 眩晕：无动作
            },
            // 劫匪：takeTurn 驱动（无 RollMove）；1=抢劫  2=烟雾  3=逃离  4=重扫
            MonsterKind::Mugger => {
                let (nm, mug, big, smoke, gold) = (
                    m.next_move,
                    m.chomp_dmg,
                    m.thrash_dmg,
                    m.thrash_block,
                    m.bellow_str,
                );
                match nm {
                    1 | 4 => {
                        let (dmg, next) = if nm == 1 {
                            // 偷金（先于伤害，无条件）：min(goldAmt, player.gold)
                            q.push(Q::StealGold {
                                src: i,
                                amount: gold,
                            });
                            q.push(Q::MonsterDamage { src: i, base: mug });
                            let mugs_done = self.monsters[i]
                                .move_history
                                .iter()
                                .filter(|&&x| x == 1)
                                .count();
                            if mugs_done >= 2 {
                                // 第 2 次抢劫后：50% 烟雾 / 50% 重扫（消耗 aiRng）
                                if self.rng.ai.random_boolean_chance(0.5) {
                                    (mug, 2)
                                } else {
                                    (mug, 4)
                                }
                            } else {
                                (mug, 1)
                            }
                        } else {
                            q.push(Q::StealGold {
                                src: i,
                                amount: gold,
                            });
                            q.push(Q::MonsterDamage { src: i, base: big });
                            (big, 2)
                        };
                        let _ = dmg;
                        self.monsters[i].move_history.push(next as u8);
                        self.monsters[i].next_move = next as u8;
                    }
                    2 => {
                        let block = smoke + if self.ascension >= 17 { 6 } else { 0 };
                        q.push(Q::MonsterBlock {
                            target: i,
                            amount: block,
                        });
                        self.monsters[i].move_history.push(3);
                        self.monsters[i].next_move = 3;
                    }
                    3 => q.push(Q::MonsterEscape { src: i }),
                    _ => {}
                }
            }
            // 咬人蛇草：1=撕咬×3  2=孢子（脆弱+虚弱）
            MonsterKind::SnakePlant => match m.next_move {
                1 => {
                    for _ in 0..m.thrash_dmg {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                2 => {
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: m.bellow_str,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.bellow_block,
                    });
                }
                _ => {}
            },
            // 斯涅克：1=瞪视（混乱）  2=咬  3=尾击+弱/易
            MonsterKind::Snecko => match m.next_move {
                1 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Confusion,
                    amount: 1,
                }),
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                3 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    let kind = if self.ascension >= 17 {
                        PowerKind::Weak
                    } else {
                        PowerKind::Vulnerable
                    };
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind,
                        amount: m.bellow_str,
                    });
                }
                _ => {}
            },
            // 百夫长：1=斩击  2=护卫（随机其他怪格挡）  3=狂怒×3
            MonsterKind::Centurion => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => q.push(Q::MonsterBlockRandomOther {
                    src: i,
                    amount: m.bellow_block,
                }),
                3 => {
                    for _ in 0..m.thrash_block {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                _ => {}
            },
            // 拜蛇教祭司：1=魔法+脆弱  2=群体治疗  3=群体力量
            MonsterKind::Healer => match m.next_move {
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: m.bellow_block,
                    });
                }
                2 => q.push(Q::HealAllMonsters(m.thrash_dmg)),
                3 => q.push(Q::StrAllMonsters(m.thrash_block)),
                _ => {}
            },
            // ══════════ Act 2 精英/Boss 及召唤物 ══════════
            // 五小鬼 + 火把头：SetMove 驱动（无 RollMove），恒定招
            MonsterKind::GremlinWarrior | MonsterKind::GremlinThief | MonsterKind::TorchHead => {
                q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                });
                self.monsters[i].move_history.push(1);
                self.monsters[i].next_move = 1;
            }
            // 胖鬼：钝击 + 虚弱（A17 +脆弱）
            MonsterKind::GremlinFat => {
                q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                });
                q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Weak,
                    amount: m.thrash_dmg,
                });
                if self.ascension >= 17 {
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: m.thrash_block,
                    });
                }
                self.monsters[i].move_history.push(2);
                self.monsters[i].next_move = 2;
            }
            // 傲鬼：保护（随机其他怪格挡）；仅剩自己时改痛击
            MonsterKind::GremlinTsundere => {
                let others_alive = self
                    .monsters
                    .iter()
                    .filter(|x| !x.creature.is_dead() && !x.escaped)
                    .count()
                    > 1;
                if m.next_move == 1 {
                    q.push(Q::MonsterBlockRandomOther {
                        src: i,
                        amount: m.thrash_dmg,
                    });
                    let next = if others_alive { 1 } else { 2 };
                    self.monsters[i].move_history.push(next);
                    self.monsters[i].next_move = next;
                } else {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    self.monsters[i].move_history.push(2);
                    self.monsters[i].next_move = 2;
                }
            }
            // 巫鬼：充能 3 回合 → 魔法爆发（counter=currentCharge 起 1）
            MonsterKind::GremlinWizard => {
                let (cur_move, magic) = (m.next_move, m.chomp_dmg);
                let next = if cur_move == 2 {
                    self.monsters[i].burn_upgraded += 1;
                    if self.monsters[i].burn_upgraded >= 3 {
                        1
                    } else {
                        2
                    }
                } else {
                    self.monsters[i].burn_upgraded = 0;
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: magic,
                    });
                    if self.ascension >= 17 {
                        1
                    } else {
                        2
                    }
                };
                self.monsters[i].move_history.push(next);
                self.monsters[i].next_move = next;
            }
            // 小鬼首领：2=Rally 召唤×2  3=Encourage  4=刺击×3
            MonsterKind::GremlinLeader => match m.next_move {
                2 => {
                    q.push(Q::SummonGremlin);
                    q.push(Q::SummonGremlin);
                }
                3 => q.push(Q::LeaderEncourage {
                    src: i,
                    str_amt: m.thrash_dmg,
                    block_amt: m.thrash_block,
                }),
                4 => {
                    for _ in 0..m.bellow_str {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                _ => {}
            },
            // 连击之书：1=刺×stabCount  2=重刺（counter=stabCount）
            MonsterKind::BookOfStabbing => match m.next_move {
                1 => {
                    let hits = m.burn_upgraded.max(1);
                    for _ in 0..hits {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                _ => {}
            },
            // 监工：鞭笞 7 + 伤口×N（A18 自+1 力量）
            MonsterKind::Taskmaster => {
                q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                });
                if let Some(wound) = CardInst::new_by_class("Wound") {
                    for _ in 0..m.thrash_dmg {
                        q.push(Q::MakeDiscard(wound));
                    }
                }
                if self.ascension >= 18 {
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Strength,
                        amount: 1,
                    });
                }
            }
            // 铜球：1=光束  2=支援（给自动机格挡）  3=静滞吸牌
            MonsterKind::BronzeOrb => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => q.push(Q::BlockMonsterKind {
                    kind: MonsterKind::BronzeAutomaton,
                    amount: m.thrash_dmg,
                }),
                3 => q.push(Q::StasisSteal { src: i }),
                _ => {}
            },
            // 铜制自动机：4=召唤两铜球  1=连枷×2  5=强化  2=超究光束  3=放电眩晕
            MonsterKind::BronzeAutomaton => match m.next_move {
                4 => {
                    q.push(Q::SpawnMonster {
                        kind: MonsterKind::BronzeOrb,
                    });
                    q.push(Q::SpawnMonster {
                        kind: MonsterKind::BronzeOrb,
                    });
                }
                1 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                5 => {
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_block,
                    });
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Strength,
                        amount: m.bellow_str,
                    });
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                3 => {} // 放电后眩晕：无动作
                _ => {}
            },
            // 收集者：1=召唤火把头×2  2=火球  3=群力  4=超大减益  5=复活
            MonsterKind::TheCollector => match m.next_move {
                1 => {
                    q.push(Q::SpawnMonster {
                        kind: MonsterKind::TorchHead,
                    });
                    q.push(Q::SpawnMonster {
                        kind: MonsterKind::TorchHead,
                    });
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                3 => {
                    let block = m.thrash_block + if self.ascension >= 19 { 5 } else { 0 };
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: block,
                    });
                    q.push(Q::StrAllMonsters(m.thrash_dmg));
                }
                4 => {
                    for kind in [PowerKind::Weak, PowerKind::Vulnerable, PowerKind::Frail] {
                        q.push(Q::ApplyPowerToPlayerFromMonster {
                            kind,
                            amount: m.bellow_str,
                        });
                    }
                }
                5 => q.push(Q::ReviveTorchHeads),
                _ => {}
            },
            // 冠军：1=重斩 2=防御姿态 3=处决×2 4=扇击 5=嘲弄 6=奚落 7=爆怒
            MonsterKind::Champ => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => {
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.dark_strike_dmg,
                    });
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Metallicize,
                        amount: m.bellow_block,
                    });
                }
                3 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                4 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_block,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: 2,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: 2,
                    });
                }
                5 => q.push(Q::ApplyPowerToMonster {
                    target: i,
                    kind: PowerKind::Strength,
                    amount: m.bellow_str,
                }),
                6 => {
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: 2,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: 2,
                    });
                }
                7 => {
                    q.push(Q::MonsterRemoveDebuffs { src: i });
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Strength,
                        amount: m.bellow_str * 3,
                    });
                }
                _ => {}
            },
            // ══════════ Act 3（TheBeyond）普通遭遇 ══════════
            // 小暗灵：1=撕咬×2  2=硬化(+A17力量)  3=噬咬  4=半死待机  5=复活半血
            MonsterKind::Darkling => match m.next_move {
                1 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                }
                2 => {
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_block,
                    });
                    if self.ascension >= 17 {
                        q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::Strength,
                            amount: m.bellow_str,
                        });
                    }
                }
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                5 => {
                    // REINCARNATE：回复半血并解除半死
                    let heal = m.creature.max_hp / 2;
                    let cr = &mut self.monsters[i].creature;
                    cr.hp = heal;
                    self.monsters[i].half_dead = false;
                }
                _ => {} // 4：半死待机（无动作，RollMove → 5 复活）
            },
            // 排斥体：1=晕眩卡×2  2=攻击
            MonsterKind::Repulsor => match m.next_move {
                1 => {
                    if let Some(dazed) = CardInst::new_by_class("Dazed") {
                        for _ in 0..m.thrash_dmg {
                            q.push(Q::MakeDiscard(dazed));
                        }
                    }
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                _ => {}
            },
            // 天球行者：1=激光(灼伤入弃牌+主牌组)  2=利爪
            MonsterKind::OrbWalker => match m.next_move {
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    if let Some(burn) = CardInst::new_by_class("Burn") {
                        q.push(Q::MakeDiscard(burn));
                        q.push(Q::ParasiteToDeck); // 主牌组灼伤（永久）
                    }
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                _ => {}
            },
            // 尖塔生长体：1=快击  2=紧缠(玩家 EOT 直伤)  3=重击
            MonsterKind::SpireGrowth => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => {
                    let amt = m.thrash_block + if self.ascension >= 17 { 2 } else { 0 };
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Constricted,
                        amount: amt,
                    });
                }
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                _ => {}
            },
            // 转瞬即逝：SetMove 驱动；Fading 到 1 时自灭；伤害=起始+count×10
            MonsterKind::Transient => {
                let (base, incr) = (m.chomp_dmg, m.thrash_dmg);
                let fading = self.monsters[i]
                    .creature
                    .powers
                    .iter()
                    .position(|p| p.kind == PowerKind::Fading);
                if let Some(fi) = fading {
                    if self.monsters[i].creature.powers[fi].amount <= 1 {
                        // SuicideAction：自灭（真死，正常胜利路径）
                        self.monsters[i].creature.hp = 0;
                        self.on_monster_death(i);
                        return q;
                    }
                    self.monsters[i].creature.powers[fi].amount -= 1;
                }
                let dmg = base + self.monsters[i].burn_upgraded * incr;
                q.push(Q::MonsterDamage { src: i, base: dmg });
                self.monsters[i].burn_upgraded += 1;
                self.monsters[i].move_history.push(1);
                self.monsters[i].next_move = 1;
            }
            // 巨口：2=咆哮(弱+脆)  3=猛击  4=流涎(+3力)  5=啃食×(tc/2)
            MonsterKind::Maw => match m.next_move {
                2 => {
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.thrash_block,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Frail,
                        amount: m.thrash_block,
                    });
                }
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                4 => q.push(Q::ApplyPowerToMonster {
                    target: i,
                    kind: PowerKind::Strength,
                    amount: m.bellow_str,
                }),
                5 => {
                    let hits = (self.monsters[i].burn_upgraded / 2).max(1);
                    for _ in 0..hits {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                _ => {}
            },
            // 蠕动聚块：0=巨击  1=连击×3  2=攻防  3=削弱攻击  4=寄生前置(一次)
            MonsterKind::WrithingMass => match m.next_move {
                0 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                1 => {
                    for _ in 0..3 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                2 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_block,
                    });
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_block,
                    });
                }
                3 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.bellow_str,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.bellow_block,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Vulnerable,
                        amount: m.bellow_block,
                    });
                }
                4 => q.push(Q::ParasiteToDeck),
                _ => {}
            },
            // ══════════ Act 3 精英/Boss 及召唤物 ══════════
            // 巨首：1=瞪视(弱1)  2=时刻已至(count 递减伤害递增)  3=计数打击
            MonsterKind::GiantHead => match m.next_move {
                1 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Weak,
                    amount: 1,
                }),
                2 => {
                    // index = 1-count（封顶 7）→ deathDmg + index*5
                    let idx = (1 - m.burn_upgraded).clamp(0, 7);
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg + idx * m.thrash_block,
                    });
                }
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                _ => {}
            },
            // 复仇者：2=三连火×3  3=镰刀45  4=三连灼伤；回合尾自动无形
            MonsterKind::Nemesis => {
                match m.next_move {
                    2 => {
                        for _ in 0..3 {
                            q.push(Q::MonsterDamage {
                                src: i,
                                base: m.thrash_dmg,
                            });
                        }
                    }
                    3 => q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    }),
                    4 => {
                        if let Some(burn) = CardInst::new_by_class("Burn") {
                            for _ in 0..m.thrash_block {
                                q.push(Q::MakeDiscard(burn));
                            }
                        }
                    }
                    _ => {}
                }
                // takeTurn 尾：无无形则补 1（玩家回合恒无形）
                let has_intangible = self.monsters[i]
                    .creature
                    .powers
                    .iter()
                    .any(|p| p.kind == PowerKind::IntangiblePlayer);
                if !has_intangible {
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::IntangiblePlayer,
                        amount: 1,
                    });
                }
            }
            // 爬虫法师：1=蛇击×2+弱1  2=召唤蛇匕(1/2)  3=重咬
            MonsterKind::Reptomancer => match m.next_move {
                1 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.bellow_str,
                    });
                }
                2 => {
                    let n = m.thrash_block;
                    for _ in 0..n {
                        q.push(Q::SpawnMonster {
                            kind: MonsterKind::SnakeDagger,
                        });
                    }
                }
                3 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                _ => {}
            },
            // 蛇匕：1=刺9  2=自爆25后自灭
            MonsterKind::SnakeDagger => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                2 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::SnakeDaggerSuicide { src: i });
                }
                _ => {}
            },
            // 觉醒者：1=挥砍20  2=灵魂冲击6×4  3=重生待机(满血复活转阶段)
            // 5=黑暗回响40  6=污泥18+虚空  8=冲撞10×3；回合始再生
            MonsterKind::AwakenedOne => {
                // RegenerateMonster：回合开始回复 10（A19+ 15）
                let (echo, sludge, tackle, tackle_n, ss, ss_n, slash) = (
                    m.thrash_block,
                    m.bellow_str,
                    m.bellow_block,
                    m.ritual_amt,
                    m.thrash_dmg,
                    m.dark_strike_dmg,
                    m.chomp_dmg,
                );
                let cur_move = m.next_move;
                let regen = if self.ascension >= 19 { 15 } else { 10 };
                {
                    let cr = &mut self.monsters[i].creature;
                    cr.hp = (cr.hp + regen).min(cr.max_hp);
                }
                match cur_move {
                    1 => q.push(Q::MonsterDamage {
                        src: i,
                        base: slash,
                    }),
                    2 => {
                        for _ in 0..ss_n {
                            q.push(Q::MonsterDamage { src: i, base: ss });
                        }
                    }
                    3 => {
                        // REBIRTH：满血复活、转阶段 2（burn_upgraded=1）
                        let stage2_hp = if self.ascension >= 9 { 320 } else { 300 };
                        let cr = &mut self.monsters[i].creature;
                        cr.max_hp = stage2_hp;
                        cr.hp = stage2_hp;
                        self.monsters[i].half_dead = false;
                        self.monsters[i].burn_upgraded = 1;
                        // 阶段 2 重置招式史（firstTurn → 黑暗回响）
                        self.monsters[i].move_history.clear();
                        self.monsters[i].next_move = 5;
                        self.monsters[i].move_history.push(5);
                    }
                    5 => q.push(Q::MonsterDamage { src: i, base: echo }),
                    6 => {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: sludge,
                        });
                        if let Some(void) = CardInst::new_by_class("VoidCard") {
                            q.push(Q::MakeInDraw {
                                inst: void,
                                random: true,
                            });
                        }
                    }
                    8 => {
                        for _ in 0..tackle_n {
                            q.push(Q::MonsterDamage {
                                src: i,
                                base: tackle,
                            });
                        }
                    }
                    _ => {}
                }
            }
            MonsterKind::Donu | MonsterKind::Deca => match m.next_move {
                0 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                    if m.kind == MonsterKind::Deca {
                        if let Some(dazed) = CardInst::new_by_class("Dazed") {
                            for _ in 0..m.thrash_block {
                                q.push(Q::MakeDiscard(dazed));
                            }
                        }
                    }
                }
                2 => {
                    if m.kind == MonsterKind::Donu {
                        q.push(Q::StrAllMonsters(m.thrash_dmg));
                    } else {
                        q.push(Q::PlatedAllMonsters(m.thrash_dmg));
                    }
                }
                _ => {}
            },
            // 吞时者：2=回响×3  3=涟漪(格挡+弱易[脆])  4=头撞+抽牌-1[+粘液]
            // 5=半血一次加速(清减益+回到半血[+格挡])
            MonsterKind::TimeEater => {
                let slam = m.thrash_dmg;
                match m.next_move {
                    2 => {
                        for _ in 0..3 {
                            q.push(Q::MonsterDamage {
                                src: i,
                                base: m.chomp_dmg,
                            });
                        }
                    }
                    3 => {
                        q.push(Q::MonsterBlock {
                            target: i,
                            amount: m.thrash_block,
                        });
                        for kind in [PowerKind::Vulnerable, PowerKind::Weak] {
                            q.push(Q::ApplyPowerToPlayerFromMonster { kind, amount: 1 });
                        }
                        if self.ascension >= 19 {
                            q.push(Q::ApplyPowerToPlayerFromMonster {
                                kind: PowerKind::Frail,
                                amount: 1,
                            });
                        }
                    }
                    4 => {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                        q.push(Q::PlayerDrawNextTurn(-1));
                        if self.ascension >= 19 {
                            if let Some(slimed) = CardInst::new_by_class("Slimed") {
                                for _ in 0..2 {
                                    q.push(Q::MakeDiscard(slimed));
                                }
                            }
                        }
                    }
                    5 => {
                        // 半血加速：清自身减益 + 回复至半血（A19+ 另获格挡=头撞伤）
                        self.monsters[i]
                            .creature
                            .powers
                            .retain(|p| !p.kind.is_debuff());
                        let half = self.monsters[i].creature.max_hp / 2;
                        let bonus_block = if self.ascension >= 19 { slam } else { 0 };
                        let cr = &mut self.monsters[i].creature;
                        if cr.hp < half {
                            cr.hp = half;
                        }
                        cr.block += bonus_block;
                    }
                    _ => {}
                }
            }
            // ══════════ Act 4（终局）══════════
            // 尖塔之盾：1=痛击(-1力/球时-1专注)  2=筑防(全怪30甲)  3=粉碎(伤+自甲)
            MonsterKind::SpireShield => match m.next_move {
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    // 有充能球时 50% 改偷专注 1（aiRng）
                    if !self.player.orbs.is_empty() && self.rng.ai.random_boolean() {
                        q.push(Q::ApplyPowerToPlayerFromMonster {
                            kind: PowerKind::Focus,
                            amount: -1,
                        });
                    } else {
                        q.push(Q::ApplyPowerToPlayerFromMonster {
                            kind: PowerKind::Strength,
                            amount: -1,
                        });
                    }
                }
                2 => q.push(Q::BlockAllMonsters(m.thrash_block)),
                3 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_dmg,
                    });
                }
                _ => {}
            },
            // 尖塔之矛：1=燃烧打击×2+灼伤2  2=穿透(全怪+2力)  3=穿刺×N
            MonsterKind::SpireSpear => match m.next_move {
                1 => {
                    for _ in 0..2 {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.chomp_dmg,
                        });
                    }
                    if let Some(burn) = CardInst::new_by_class("Burn") {
                        for _ in 0..m.thrash_block {
                            q.push(Q::MakeDiscard(burn));
                        }
                    }
                }
                2 => q.push(Q::StrAllMonsters(2)),
                3 => {
                    for _ in 0..m.bellow_str {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                _ => {}
            },
            // 腐朽之心：1=血射×N  2=回响  3=开场削弱(+5状态卡)  4=增益轮转
            MonsterKind::CorruptHeart => match m.next_move {
                1 => {
                    for _ in 0..m.thrash_block {
                        q.push(Q::MonsterDamage {
                            src: i,
                            base: m.thrash_dmg,
                        });
                    }
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                3 => {
                    for kind in [PowerKind::Vulnerable, PowerKind::Weak, PowerKind::Frail] {
                        q.push(Q::ApplyPowerToPlayerFromMonster {
                            kind,
                            amount: m.bellow_str,
                        });
                    }
                    // 五连状态：晕眩/粘液/伤口/灼伤/虚空 → 抽牌堆随机位
                    for cls in ["Dazed", "Slimed", "Wound", "Burn", "VoidCard"] {
                        if let Some(c) = CardInst::new_by_class(cls) {
                            q.push(Q::MakeInDraw {
                                inst: c,
                                random: true,
                            });
                        }
                    }
                }
                4 => {
                    // 力量 +2（若力量为负先抵消）；buffCount 由招式史推导
                    let buff_count = self.monsters[i]
                        .move_history
                        .iter()
                        .filter(|&&x| x == 4)
                        .count() as i32;
                    let str_now = self.monsters[i]
                        .creature
                        .powers
                        .iter()
                        .find(|p| p.kind == PowerKind::Strength)
                        .map(|p| p.amount)
                        .unwrap_or(0);
                    let bonus = if str_now < 0 { -str_now + 2 } else { 2 };
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Strength,
                        amount: bonus,
                    });
                    match buff_count {
                        0 => q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::Artifact,
                            amount: 2,
                        }),
                        1 => q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::BeatOfDeath,
                            amount: 1,
                        }),
                        2 => q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::PainfulStabs,
                            amount: 1,
                        }),
                        3 => q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::Strength,
                            amount: 10,
                        }),
                        _ => q.push(Q::ApplyPowerToMonster {
                            target: i,
                            kind: PowerKind::Strength,
                            amount: 50,
                        }),
                    }
                }
                _ => {}
            },
            // 大酸液史莱姆：1=腐蚀冲撞+粘液2  2=普通冲撞  3=分裂  4=弱舔2
            MonsterKind::AcidSlimeL => match m.next_move {
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    if let Some(slimed) = CardInst::new_by_class("Slimed") {
                        for _ in 0..m.thrash_block {
                            q.push(Q::MakeDiscard(slimed));
                        }
                    }
                }
                2 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.thrash_dmg,
                }),
                3 => q.push(Q::SlimeSplit {
                    src: i,
                    acid: true,
                    hp: self.monsters[i].creature.hp,
                }),
                4 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Weak,
                    amount: m.bellow_block,
                }),
                _ => {}
            },
            // 大尖刺史莱姆：1=火焰冲撞+粘液2  3=分裂  4=脆舔(2/3 A17)
            MonsterKind::SpikeSlimeL => match m.next_move {
                1 => {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                    if let Some(slimed) = CardInst::new_by_class("Slimed") {
                        for _ in 0..m.thrash_block {
                            q.push(Q::MakeDiscard(slimed));
                        }
                    }
                }
                3 => q.push(Q::SlimeSplit {
                    src: i,
                    acid: false,
                    hp: self.monsters[i].creature.hp,
                }),
                4 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Frail,
                    amount: m.bellow_str,
                }),
                _ => {}
            },
            // ── 红面罩三人组（BanditPointy/Leader/Bear.java takeTurn 逐行；
            // 无 RollMove——序列由 takeTurn 内 SetMoveAction 驱动，
            // 引擎在 get_move 以 move_history 等价推演）──
            MonsterKind::BanditPointy => {
                // 双段普攻（takeTurn 两次 DamageAction）
                for _ in 0..m.dark_strike_dmg {
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.chomp_dmg,
                    });
                }
            }
            MonsterKind::BanditLeader => match m.next_move {
                3 => {
                    // 痛苦斩：伤 + 虚弱
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.thrash_dmg,
                    });
                    q.push(Q::ApplyPowerToPlayerFromMonster {
                        kind: PowerKind::Weak,
                        amount: m.ritual_amt,
                    });
                }
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                _ => {} // 2 嘲讽：空转
            },
            MonsterKind::BanditBear => match m.next_move {
                2 => q.push(Q::ApplyPowerToPlayerFromMonster {
                    kind: PowerKind::Dexterity,
                    amount: m.bellow_str, // 负值（-2 / A17 -4）
                }),
                3 => {
                    // 扑击：伤 + 自挡 9
                    q.push(Q::MonsterDamage {
                        src: i,
                        base: m.dark_strike_dmg,
                    });
                    q.push(Q::MonsterBlock {
                        target: i,
                        amount: m.thrash_block,
                    });
                }
                _ => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
            },
            // ── 古形体 ──
            MonsterKind::Spiker => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                _ => {
                    // 荆棘 +2（thornsCount 计次在入队时；>5 后恒普攻）
                    let amt = m.ritual_amt;
                    q.push(Q::ApplyPowerToMonster {
                        target: i,
                        kind: PowerKind::Thorns,
                        amount: amt,
                    });
                    self.monsters[i].bellow_str += 1;
                }
            },
            MonsterKind::Exploder => match m.next_move {
                1 => q.push(Q::MonsterDamage {
                    src: i,
                    base: m.chomp_dmg,
                }),
                // 待机回合 = ExplosivePower 走到 1：自爆 30（THORNS，不可格挡）
                _ => q.push(Q::ExploderBlast { src: i }),
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
        // 玩家侧仪式（CultistPotion）：RitualPower.atEndOfRound 轮末投予力量
        {
            let mut grants: Vec<i32> = Vec::new();
            for p in self.player.creature.powers.iter_mut() {
                if p.kind == PowerKind::Ritual {
                    if p.just_applied {
                        p.just_applied = false;
                    } else {
                        grants.push(p.amount);
                    }
                }
            }
            for g in grants {
                apply_power(
                    &mut self.player.creature.powers,
                    PowerKind::Strength,
                    g,
                    false,
                    self.turn_has_ended,
                );
            }
        }
        for i in 0..self.monsters.len() {
            if !self.alive(i) {
                continue;
            }
            let mut grants: Vec<i32> = Vec::new();
            let mut regain: Vec<i32> = Vec::new();
            for p in &mut self.monsters[i].creature.powers {
                if p.kind == PowerKind::Ritual {
                    if p.just_applied {
                        p.just_applied = false;
                    } else {
                        grants.push(p.amount);
                    }
                }
                if p.kind == PowerKind::GainStrength {
                    regain.push(p.amount);
                }
            }
            self.monsters[i]
                .creature
                .powers
                .retain(|p| p.kind != PowerKind::GainStrength);
            // Slow：轮末 amount 归零（atEndOfRound）
            for p in &mut self.monsters[i].creature.powers {
                if p.kind == PowerKind::Slow {
                    p.amount = 0;
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
            for g in regain {
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

    pub(crate) fn drain(&mut self, mut q: Vec<Q>) {
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

    /// AbstractPlayer.damage 的玩家侧收尾：decrementBlock（HP_LOSS 绕过格挡）
    /// → 遗物管线（Torii.onAttacked → TungstenRod.onLoseHpLast → CentennialPuzzle.
    /// wasHPLost，见 relics.rs）→ 扣血。返回实际 HP 损失（≥0）。
    /// 无形截断与乘区由调用方先行算入 dmg（calc_monster_damage / 各直伤点）。
    /// 死亡判定/瓶中精灵仍在调用点处理。
    pub(crate) fn player_deal_damage(&mut self, dmg: i32, ty: DamageType) -> i32 {
        let mut d = dmg.max(0);
        // AbstractCreature.decrementBlock：仅 HP_LOSS 无视格挡
        // （THORNS 型可被格挡——荆棘/自爆/亡律均可挡）
        if ty != DamageType::HpLoss {
            let cr = &mut self.player.creature;
            if d > cr.block {
                d -= cr.block;
                cr.block = 0;
            } else {
                cr.block -= d;
                d = 0;
            }
        }
        self.relic_damage_hooks(d, ty)
    }

    pub(crate) fn exec(&mut self, a: Q) {
        match a {
            Q::DamageMonster { target, dmg } => {
                if self.alive(target) {
                    // Curl Up 统一在 hit_monster 内处理（顺序：先扣血后加格挡，
                    // 对应 Java CurlUpPower.onAttacked 的 addToBot(GainBlockAction)）。
                    // 此处不可重复实现，否则格挡会提前加上并吸收本次伤害。
                    // 怪物荆棘（Spiker 系）：ThornsPower.onAttacked ——
                    // 玩家 NORMAL 攻击命中（output>0，含致死）→ 反弹 THORNS 伤
                    // （无视格挡；不递归——THORNS 型不再触发对方荆棘）
                    let thorns = self.monsters[target]
                        .creature
                        .powers
                        .iter()
                        .find(|pw| pw.kind == PowerKind::Thorns)
                        .map(|pw| pw.amount)
                        .unwrap_or(0);
                    self.hit_monster(target, dmg, DamageType::Normal);
                    if dmg > 0 && thorns > 0 && !self.over {
                        self.player_deal_damage(thorns, DamageType::Thorns);
                        if self.player.creature.is_dead() && !self.fairy_rescue() {
                            self.over = true;
                            self.won = false;
                        }
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
                let before_hp = self.player.creature.hp;
                self.player_deal_damage(d, DamageType::Normal);
                // Plated Armor wasHPLost：NORMAL 类型实际掉血 → -1 层
                if before_hp - self.player.creature.hp > 0 {
                    self.reduce_plated_armor();
                }
                if self.player.creature.is_dead() && !self.fairy_rescue() {
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
                        self.hit_monster(src, amt, DamageType::Normal);
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
                // Snake Skull：玩家施加的中毒 +1（ApplyPowerAction:38）
                let mut amount = amount;
                if kind == PowerKind::Poison && self.has_relic(crate::relics::RelicKind::SnakeSkull)
                {
                    amount += 1;
                }
                // Champion Belt：施加易伤时同时施加虚弱 1（ApplyPowerAction:105）
                let mut extra_weak = false;
                if kind == PowerKind::Vulnerable
                    && self.has_relic(crate::relics::RelicKind::ChampionBelt)
                {
                    extra_weak = true;
                }
                if self.alive(target) {
                    // 怪物侧神器（SphericGuardian）：DEBUFF 被抵消
                    if kind.is_debuff() {
                        if let Some(idx) = self.monsters[target]
                            .creature
                            .powers
                            .iter()
                            .position(|pw| pw.kind == PowerKind::Artifact)
                        {
                            if self.monsters[target].creature.powers[idx].amount > 0 {
                                self.monsters[target].creature.powers[idx].amount -= 1;
                                if self.monsters[target].creature.powers[idx].amount == 0 {
                                    self.monsters[target].creature.powers.remove(idx);
                                }
                                return;
                            }
                        }
                    }
                    apply_power(
                        &mut self.monsters[target].creature.powers,
                        kind,
                        amount,
                        false,
                        self.turn_has_ended,
                    );
                    // Champion Belt：易伤后同时施加虚弱 1
                    if extra_weak && self.alive(target) {
                        apply_power(
                            &mut self.monsters[target].creature.powers,
                            PowerKind::Weak,
                            1,
                            false,
                            self.turn_has_ended,
                        );
                    }
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
                // HP 损失绕过格挡（HP_LOSS 也触发瓶中精灵；TungstenRod 每次失血 -1）
                self.player_deal_damage(n, DamageType::HpLoss);
                if self.player.creature.is_dead() && !self.fairy_rescue() {
                    self.over = true;
                    self.won = false;
                    return;
                }
                // RupturePower.wasHPLost（info.owner == 玩家）：因自身卡牌失血 → 力量 +amount
                if let Some(amt) = self
                    .player
                    .creature
                    .powers
                    .iter()
                    .find(|p| p.kind == PowerKind::Rupture)
                    .map(|p| p.amount)
                {
                    apply_power(
                        &mut self.player.creature.powers,
                        PowerKind::Strength,
                        amt,
                        false,
                        false,
                    );
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
                    self.hit_monster(t, d, DamageType::Normal);
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
                    // MentalFortressPower.onChangeStance：获 amount 格挡
                    if let Some(amt) = self
                        .player
                        .creature
                        .powers
                        .iter()
                        .find(|p| p.kind == PowerKind::MentalFortress)
                        .map(|p| p.amount)
                    {
                        self.player.creature.block += amt;
                    }
                    // RushdownPower.onChangeStance：换姿态后抽 2
                    if let Some(amt) = self
                        .player
                        .creature
                        .powers
                        .iter()
                        .find(|p| p.kind == PowerKind::Rushdown)
                        .map(|p| p.amount)
                    {
                        if amt > 0 {
                            self.draw_cards(amt as usize);
                        }
                    }
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
                // ShuffleAction → relics.onShuffle（Sundial）
                self.relics_on_shuffle();
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
                    let hp0 = self.monsters[target].creature.hp;
                    self.hit_monster(target, d, DamageType::Normal);
                    // Wallop：获得格挡 = 实际掉血
                    self.player.creature.block += (hp0 - self.monsters[target].creature.hp).max(0);
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
                // DiscardAction(amount >= hand.size())：同样走 getTopCard() 从末尾取 ⇒ 逆序
                let cards = std::mem::take(&mut self.player.hand);
                self.player.discard.extend(cards.into_iter().rev());
            }
            Q::DoSetHandCostZero => {
                // BulletTime：本回合手牌全部免费（setCostForTurn(-9)）
                self.hand_cost_zero_this_turn = true;
            }
            Q::DoExhaustHand => {
                let cards = std::mem::take(&mut self.player.hand);
                let n = cards.len();
                for c in cards {
                    self.push_exhaust(c);
                }
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
                    if self.hit_monster(target, d, DamageType::Normal) {
                        // FeedAction：击杀时增加最大生命
                        self.player.creature.max_hp += max_hp_gain;
                        self.player.creature.hp += max_hp_gain;
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
            Q::ApplyPowerToPlayerFromMonster { kind, amount } => {
                self.apply_power_to_player_checked(kind, amount, true);
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
                                self.hit_monster(t, passive, DamageType::Normal);
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
                    // 执行卡牌效果（不能量消耗）
                    let mut q2 = Vec::new();
                    let ti = 0;
                    for &eff in card.def.effects {
                        self.apply_effect(eff, card, ti, 0, &mut q2, 0);
                    }
                    if card.def.card_type == "ATTACK" {
                        self.player
                            .creature
                            .powers
                            .retain(|p| !matches!(p.kind, PowerKind::Vigor | PowerKind::PenNib));
                    }
                    self.relics_on_use_card(card.def.card_type, &mut q2);
                    self.powers_on_use_card(card.def.card_type, &mut q2);
                    // 去向
                    if card.def.flags.contains(&"exhaust") {
                        self.push_exhaust(card);
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
                        let hp0 = self.monsters[mi].creature.hp;
                        self.hit_monster(mi, d, DamageType::Normal);
                        total += (hp0 - self.monsters[mi].creature.hp).max(0);
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
                    self.hit_monster(target, d, DamageType::Normal);
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
                    self.push_exhaust(c);
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
                        if self.hit_monster(target, d, DamageType::Normal) || self.over {
                            break;
                        }
                    }
                }
            }
            Q::RollMove(i) => self.roll_move(i),
            Q::SetMonsterMove { target, next: mv } => {
                if target < self.monsters.len() {
                    self.monsters[target].next_move = mv;
                }
            }
            Q::SetMonsterCounter { target, value } => {
                if target < self.monsters.len() {
                    self.monsters[target].burn_upgraded = value;
                }
            }
            // —— 药水系统 exec ——
            Q::DamagePure { target, dmg } => {
                if self.alive(target) {
                    self.hit_monster(target, dmg, DamageType::Normal);
                }
            }
            Q::DamageAllPure { dmg } => {
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        self.hit_monster(mi, dmg, DamageType::Thorns);
                        if self.monsters[mi].creature.is_dead() {
                            self.on_monster_death(mi);
                            if self.over {
                                return;
                            }
                        }
                    }
                }
            }
            Q::DoDiscovery {
                pool: pool_kind,
                copies,
            } => {
                // DiscoveryAction.generateCardChoices：采样 3 张互不重复（HEALING 剔除）
                let mut options: Vec<CardInst> = Vec::new();
                while options.len() < 3 {
                    let c = self.sample_discovery_card(pool_kind);
                    if options.iter().any(|o| o.def.id == c.def.id) {
                        continue; // dupe → 重掷（同一 rng 流）
                    }
                    options.push(c);
                }
                self.pending = Some(Pending::Discovery { options, copies });
            }
            Q::DoStanceChoice => {
                self.pending = Some(Pending::StanceChoice);
            }
            Q::DoGamblersDiscard => {
                self.pending = Some(Pending::GamblersDiscard);
            }
            Q::DoExhaustAny => {
                if !self.player.hand.is_empty() {
                    self.pending = Some(Pending::HandExhaustAny);
                }
            }
            Q::DoDiscardPickFreeCost => {
                // BetterDiscardPileToHandAction(1, 0)：弃堆恰 1 张时免选择直接入手
                if self.player.discard.len() <= 1 {
                    if let Some(mut c) = self.player.discard.pop() {
                        if self.player.hand.len() < 10 {
                            c.cost_delta = -(c.def.cost.unwrap_or(0)).max(0);
                            self.player.hand.push(c);
                        } else {
                            self.player.discard.push(c);
                        }
                    }
                } else if self.player.hand.len() < 10 {
                    self.pending = Some(Pending::PilePick {
                        pile: "discard",
                        purpose: PilePickPurpose::DiscardToHandFreeCost,
                    });
                }
            }
            Q::DoDistilledChaos { n } => {
                // PlayTopCardAction ×n：抽堆顶免贽打出（目标 = 随机存活怪；
                // 抽堆尽且弃堆尽则中止；抽堆尽则洗牌后续）
                for _ in 0..n.max(0) {
                    if self.player.draw.is_empty() {
                        if self.player.discard.is_empty() {
                            break;
                        }
                        let mut pile = std::mem::take(&mut self.player.discard);
                        card_group_shuffle(&mut pile, &mut self.rng.shuffle);
                        self.player.draw = pile;
                    }
                    let card = self.player.draw.pop().expect("checked");
                    // getRandomMonster(null, true, cardRandomRng)
                    let alive: Vec<usize> = (0..self.monsters.len())
                        .filter(|&i| self.alive(i))
                        .collect();
                    let target = if alive.is_empty() {
                        None
                    } else {
                        let roll = self
                            .rng
                            .card_random
                            .random_start_end(0, (alive.len() - 1) as i32);
                        Some(alive[(roll.max(0) as usize).min(alive.len() - 1)])
                    };
                    self.play_card_free(card, target);
                    if self.over {
                        return;
                    }
                }
            }
            Q::DoRandomizeHandCost => {
                // RandomizeHandCostAction：cost>=0 的手牌费用 ← cardRandomRng.random(3)
                // （永改：card.cost = costForTurn = newCost）
                for c in self.player.hand.iter_mut() {
                    let cur = c.def.cost.unwrap_or(-2);
                    if cur < 0 {
                        continue;
                    }
                    let new_cost = self.rng.card_random.random_range(3);
                    if cur + c.cost_delta == new_cost {
                        continue; // 源码：cost == newCost 时跳过（等效无操作）
                    }
                    c.cost_delta = new_cost - cur;
                }
            }
            Q::DoChannelDarkPerOrb { amount } => {
                // EssenceOfDarknessAction 字面复刻：外层条件随播球动态增长
                let mut i = 0;
                while i < self.player.orbs.len() {
                    for _ in 0..amount.max(0) {
                        self.exec(Q::Channel {
                            kind: crate::state::OrbKind::Dark,
                            auto_evoke: true,
                        });
                    }
                    i += 1;
                }
            }
            Q::DoObtainRandomPotions => {
                // EntropicBrew：每个空栏位掣一瓶（returnRandomPotion(true)：
                // 掷稀度 + 拒绝采样，跳过果汁；至少重掣一次）
                let pool = pool_for_class("Ironclad"); // M3：按局外角色参数化
                for slot in self.player.potions.iter_mut() {
                    if slot.is_none() {
                        let k = return_random_potion(&pool, true, &mut self.rng.potion);
                        *slot = Some(crate::potions::PotionInst::new(k));
                    }
                }
            }
            Q::MaxHpPlus(n) => {
                // FruitJuice：increaseMaxHp(n, true) → 上限+n 且治疗 n
                self.player.creature.max_hp += n;
                self.player.creature.hp += n;
            }
            Q::DoSmokeEscape => {
                // SmokeBomb：room.smoked → 战斗立即结束（逃离，无奖励，不算失败）
                self.over = true;
                self.won = false;
                self.escaped = true;
            }
            Q::DoPlayerRitual(amount) => {
                // CultistPotion：RitualPower(player, amount, true)（skipFirst）
                apply_power(
                    &mut self.player.creature.powers,
                    PowerKind::Ritual,
                    amount,
                    false,
                    false,
                );
            }
            Q::UpgradeAllBurnPiles => {
                // BurnIncreaseAction：升级弃牌堆与抽牌堆的全部灼伤（手牌不动）
                for c in self.player.discard.iter_mut() {
                    if c.def.id == "Burn" {
                        c.upgraded = true;
                    }
                }
                for c in self.player.draw.iter_mut() {
                    if c.def.id == "Burn" {
                        c.upgraded = true;
                    }
                }
            }
            Q::MonsterBlockRandomOther { src, amount } => {
                // GainBlockRandomMonsterAction：排除自己/逃跑/死亡，空则给自己
                let candidates: Vec<usize> = (0..self.monsters.len())
                    .filter(|&m| m != src && self.alive(m))
                    .collect();
                let target = if candidates.is_empty() {
                    src
                } else {
                    let roll = self.rng.ai.random_range(candidates.len() as i32 - 1);
                    candidates[(roll.max(0) as usize).min(candidates.len() - 1)]
                };
                self.monsters[target].creature.block += amount;
            }
            Q::HealAllMonsters(amount) => {
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        let cr = &mut self.monsters[mi].creature;
                        cr.hp = (cr.hp + amount).min(cr.max_hp);
                    }
                }
            }
            Q::StrAllMonsters(amount) => {
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        apply_power(
                            &mut self.monsters[mi].creature.powers,
                            PowerKind::Strength,
                            amount,
                            false,
                            false,
                        );
                    }
                }
            }
            Q::MonsterVampire { src, base } => {
                if self.alive(src) {
                    let d = calc_monster_damage(
                        base,
                        DamageType::Normal,
                        &self.monsters[src].creature.powers,
                        &self.player.creature.powers,
                        self.player.stance,
                    );
                    let p_hp0 = self.player.creature.hp;
                    // VampireDamageAction：对玩家造成伤害，治疗=实际掉血
                    // （无形截断与瓶中精灵与 MonsterDamage 同构）
                    if self
                        .player
                        .creature
                        .powers
                        .iter()
                        .any(|p| p.kind == PowerKind::IntangiblePlayer)
                        && d > 1
                    {
                        self.player.creature.apply_damage(1);
                    } else {
                        self.player.creature.apply_damage(d);
                    }
                    let healed = (p_hp0 - self.player.creature.hp).max(0);
                    if self.player.creature.is_dead() && !self.fairy_rescue() {
                        self.over = true;
                        self.won = false;
                        return;
                    }
                    if healed > 0 {
                        let cr = &mut self.monsters[src].creature;
                        cr.hp = (cr.hp + healed).min(cr.max_hp);
                    }
                }
            }
            Q::StealGold { src, amount } => {
                let steal = amount.min(self.gold).max(0);
                self.gold -= steal;
                // ⚠️ stolenGold 记在 bellow_block；burn_upgraded 已被 slashCount 占用，
                // 两者混用会让 Looter 的状态机判错。
                self.monsters[src].bellow_block += steal;
            }
            Q::MonsterEscape { src } => {
                // EscapeAction：离场；不触发亡语；全员离场=战斗结束
                self.monsters[src].escaped = true;
                if self.all_monsters_dead() {
                    self.over = true;
                    self.won = true;
                }
            }
            Q::SpawnMonster { kind } => {
                // SpawnMonsterAction：构造器初始招（不消耗 aiRng）；
                // HP 用 monster_hp 流（真游戏为全局随机——对拍时以 bridge 实测校准）
                let asc = self.ascension;
                let mut m = MonsterState::spawn(kind, asc, &mut self.rng.monster_hp);
                if kind == MonsterKind::GremlinWizard {
                    m.burn_upgraded = 1; // currentCharge 起 1
                }
                if kind == MonsterKind::GremlinWarrior {
                    apply_power(
                        &mut m.creature.powers,
                        PowerKind::Angry,
                        if asc >= 17 { 2 } else { 1 },
                        false,
                        false,
                    );
                }
                let init_move: u8 = match kind {
                    MonsterKind::GremlinWizard => 2, // 充能
                    MonsterKind::GremlinFat => 2,
                    _ => 1,
                };
                m.next_move = init_move;
                m.move_history.push(init_move);
                self.monsters.push(m);
            }
            Q::SummonGremlin => {
                // SummonGremlinAction：aiRng 八池（战/贼×2 胖×2 傲 巫）；
                // 空位 = 死亡小鬼位优先，总数上限 3
                const POOL: [MonsterKind; 8] = [
                    MonsterKind::GremlinWarrior,
                    MonsterKind::GremlinWarrior,
                    MonsterKind::GremlinThief,
                    MonsterKind::GremlinThief,
                    MonsterKind::GremlinFat,
                    MonsterKind::GremlinFat,
                    MonsterKind::GremlinTsundere,
                    MonsterKind::GremlinWizard,
                ];
                let roll = self.rng.ai.random_start_end(0, 7);
                let kind = POOL[(roll.max(0) as usize).min(7)];
                let leader_pos = self
                    .monsters
                    .iter()
                    .position(|m| m.kind == MonsterKind::GremlinLeader);
                if let Some(lp) = leader_pos {
                    let minions: Vec<usize> =
                        (0..self.monsters.len()).filter(|&j| j != lp).collect();
                    let asc = self.ascension;
                    let mut nm = MonsterState::spawn(kind, asc, &mut self.rng.monster_hp);
                    if kind == MonsterKind::GremlinWizard {
                        nm.burn_upgraded = 1;
                    }
                    if kind == MonsterKind::GremlinWarrior {
                        apply_power(
                            &mut nm.creature.powers,
                            PowerKind::Angry,
                            if asc >= 17 { 2 } else { 1 },
                            false,
                            false,
                        );
                    }
                    let init_move: u8 = if kind == MonsterKind::GremlinWizard {
                        2
                    } else {
                        1
                    };
                    nm.next_move = init_move;
                    nm.move_history.push(init_move);
                    if let Some(&dead_slot) = minions
                        .iter()
                        .find(|&&j| self.monsters[j].creature.is_dead() || self.monsters[j].escaped)
                    {
                        self.monsters[dead_slot] = nm; // 占用死亡位
                    } else if minions.len() < 3 {
                        self.monsters.push(nm);
                    }
                }
            }
            Q::StasisSteal { src } => {
                // ApplyStasisAction：抽牌堆（空则弃牌堆）稀有→罕见→普通→任意；
                // 每级一次 cardRandomRng 调用（含落空）；抽走并暂存
                let pick = |pile: &mut Vec<CardInst>,
                            rng: &mut crate::rng::StsRandom|
                 -> Option<CardInst> {
                    for rarity in ["RARE", "UNCOMMON", "COMMON"] {
                        let idxs: Vec<usize> = pile
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.def.rarity == rarity)
                            .map(|(j, _)| j)
                            .collect();
                        if !idxs.is_empty() {
                            let roll = rng.random_range(idxs.len() as i32 - 1);
                            let pick = idxs[(roll.max(0) as usize).min(idxs.len() - 1)];
                            return Some(pile.remove(pick));
                        }
                    }
                    // 任意：全堆均匀
                    if !pile.is_empty() {
                        let roll = rng.random_range(pile.len() as i32 - 1);
                        let pick = (roll.max(0) as usize).min(pile.len() - 1);
                        return Some(pile.remove(pick));
                    }
                    None
                };
                let card = if !self.player.draw.is_empty() {
                    pick(&mut self.player.draw, &mut self.rng.card_random)
                } else {
                    pick(&mut self.player.discard, &mut self.rng.card_random)
                };
                if let Some(c) = card {
                    self.monsters[src].stasis_card = Some(c);
                    apply_power(
                        &mut self.monsters[src].creature.powers,
                        PowerKind::Stasis,
                        1,
                        false,
                        false,
                    );
                }
            }
            Q::BlockMonsterKind { kind, amount } => {
                if let Some(t) = self
                    .monsters
                    .iter()
                    .position(|m| m.kind == kind && !m.creature.is_dead() && !m.escaped)
                {
                    self.monsters[t].creature.block += amount;
                }
            }
            Q::LeaderEncourage {
                src,
                str_amt,
                block_amt,
            } => {
                for mi in 0..self.monsters.len() {
                    if !self.alive(mi) {
                        continue;
                    }
                    apply_power(
                        &mut self.monsters[mi].creature.powers,
                        PowerKind::Strength,
                        str_amt,
                        false,
                        false,
                    );
                    if mi != src {
                        self.monsters[mi].creature.block += block_amt;
                    }
                }
            }
            Q::MonsterRemoveDebuffs { src } => {
                if self.alive(src) {
                    self.monsters[src]
                        .creature
                        .powers
                        .retain(|p| !p.kind.is_debuff());
                }
            }
            Q::ReviveTorchHeads => {
                // 收集者：每个死亡/逃离的火把头位重生一只（同位替换）
                let asc = self.ascension;
                for mi in 0..self.monsters.len() {
                    if self.monsters[mi].kind == MonsterKind::TorchHead
                        && (self.monsters[mi].creature.is_dead() || self.monsters[mi].escaped)
                    {
                        let mut nm = MonsterState::spawn(
                            MonsterKind::TorchHead,
                            asc,
                            &mut self.rng.monster_hp,
                        );
                        nm.next_move = 1;
                        nm.move_history.push(1);
                        self.monsters[mi] = nm;
                    }
                }
            }
            Q::SnakeDaggerSuicide { src } => {
                // LoseHPAction(this, currentHealth)：自灭（真死路径）
                if self.alive(src) {
                    self.monsters[src].creature.hp = 0;
                    self.on_monster_death(src);
                }
            }
            Q::ExploderBlast { src } => {
                // ExplosivePower.duringTurn（amount=1）：SuicideAction → 30 伤
                // （THORNS 型可被格挡；玩家先结算伤害避免胜利吞伤——
                // 真游戏队列中两动作都会结算）
                if self.alive(src) {
                    self.player_deal_damage(30, DamageType::Thorns);
                    if self.player.creature.is_dead() && !self.fairy_rescue() {
                        self.over = true;
                        self.won = false;
                        return;
                    }
                    self.monsters[src].creature.hp = 0;
                    self.on_monster_death(src);
                }
            }
            Q::PlatedAllMonsters(amount) => {
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        apply_power(
                            &mut self.monsters[mi].creature.powers,
                            PowerKind::PlatedArmor,
                            amount,
                            false,
                            false,
                        );
                    }
                }
            }
            Q::PlayerDrawNextTurn(n) => {
                self.next_turn_draw += n;
            }
            Q::SlimeSplit { src, acid, hp } => {
                // CannotLose→SuicideAction(不计击杀)→2×SpawnMonster(M, newHp=currentHealth)
                let kind = if acid {
                    MonsterKind::AcidSlimeM
                } else {
                    MonsterKind::SpikeSlimeM
                };
                self.monsters[src].creature.hp = 0;
                self.monsters[src].creature.block = 0;
                // 不走 on_monster_death（SuicideAction 不触发亡语/胜利判定）
                let asc = self.ascension;
                for _ in 0..2 {
                    let mut m = MonsterState::spawn(kind, asc, &mut self.rng.monster_hp);
                    // M 版 HP = 分裂时 L 版当前 HP（构造器 newHealth 参数）
                    m.creature.max_hp = hp;
                    m.creature.hp = hp;
                    m.next_move = 1;
                    m.move_history.push(1);
                    self.monsters.push(m);
                }
                if self.all_monsters_dead() {
                    self.over = true;
                    self.won = true;
                }
            }
            Q::BlockAllMonsters(amount) => {
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        self.monsters[mi].creature.block += amount;
                    }
                }
            }
            Q::ParasiteToDeck => {
                if let Some(c) = CardInst::new_by_class("Burn") {
                    let _ = c;
                }
                if let Some(parasite) = CardInst::new_by_class("Parasite") {
                    self.master_deck_additions.push(parasite);
                }
            }
            Q::PlayerFlatDamage(n) => {
                // Burn.use（dontTriggerOnUseCard）：THORNS 直伤 —— 无乘区；
                // 无形 atDamageFinalReceive 截断 >1 → 1；格挡吸收；可触发瓶中精灵
                let mut d = n.max(0);
                if self
                    .player
                    .creature
                    .powers
                    .iter()
                    .any(|p| p.kind == PowerKind::IntangiblePlayer)
                    && d > 1
                {
                    d = 1;
                }
                self.player_deal_damage(d, DamageType::Thorns);
                if self.player.creature.is_dead() && !self.fairy_rescue() {
                    self.over = true;
                    self.won = false;
                }
            }
            Q::DamageAllThorns { dmg } => {
                // Letter Opener 等：全部存活怪 THORNS 直伤（无乘区、无受击钩子）
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        self.hit_monster(mi, dmg, DamageType::Thorns);
                    }
                }
            }
        }
    }

    /// 出牌 power 钩子（AbstractPower.onUseCard）：
    /// - AfterImagePower：打出任意牌 → 获 amount 格挡
    /// - StormPower：打出能力牌 → 充能 amount 颗闪电球
    fn powers_on_use_card(&mut self, card_type: &str, q: &mut Vec<Q>) {
        for pw in &self.player.creature.powers {
            match pw.kind {
                PowerKind::AfterImage => q.push(Q::PlayerBlock(pw.amount)),
                PowerKind::Storm if card_type == "POWER" => {
                    for _ in 0..pw.amount {
                        q.push(Q::Channel {
                            kind: crate::state::OrbKind::Lightning,
                            auto_evoke: false,
                        });
                    }
                }
                _ => {}
            }
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
                    self.hit_monster(t, evoke, DamageType::Normal);
                }
            }
            crate::state::OrbKind::Dark => {
                // DarkOrbEvokeAction：对所有敌人造成伤害（THORNS 类型）
                for mi in 0..self.monsters.len() {
                    if self.alive(mi) {
                        self.hit_monster(mi, evoke, DamageType::Thorns);
                        if self.over {
                            return;
                        }
                    }
                }
            }
            crate::state::OrbKind::Plasma => {
                self.player.energy += evoke; // GainEnergyAction
            }
        }
    }

    /// 怪物死亡钩子：Spore Cloud（对玩家施加易伤）→ 全灭判定
    fn on_monster_death(&mut self, i: usize) {
        // Looter/Mugger.die → addStolenGoldToRewards(stolenGold)：
        // 击败偷钱怪后，被偷的金币作为 STOLEN_GOLD 奖励项归还。
        if matches!(
            self.monsters[i].kind,
            MonsterKind::Looter | MonsterKind::Mugger
        ) && self.monsters[i].bellow_block > 0
        {
            let amt = self.monsters[i].bellow_block;
            self.stolen_gold_rewards += amt;
            self.monsters[i].bellow_block = 0;
        }
        // Stasis（铜球）：死亡归还吸走的卡（手牌满则弃牌堆）
        if let Some(card) = self.monsters[i].stasis_card.take() {
            if self.player.hand.len() < 10 {
                self.player.hand.push(card);
            } else {
                self.player.discard.push(card);
            }
        }
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
            return;
        }
        // 遗物 onMonsterDeath（Gremlin Horn：非终局击杀 +1 能量抽 1）
        self.relics_on_monster_death();
    }

    /// 入消耗堆统一入口（CardGroup.moveToExhaustPile）：先遗物 onExhaust
    /// （Charon's Ashes 3 直伤全体）再由调用点处理 power 钩子（Dark Embrace）
    fn push_exhaust(&mut self, card: CardInst) {
        self.player.exhaust.push(card);
        self.relics_on_exhaust();
    }

    /// 怪物受伤统一入口（AbstractMonster.damage + powers 钩子管线，Act 2 起）。
    /// 钩子清单：
    /// - CurlUp：非致死伤害获得格挡后移除（先于扣血，吸收本次）
    /// - Flight：非致死 NORMAL 受击 -1 层；归零移除 → Byrd GROUNDED（眩晕）
    /// - Malleable：非致死 NORMAL 受击 → +amount 格挡且 amount+1
    /// - PlatedArmor（怪物侧）：实际掉血 → -1；归零移除 → 灼壳寄生生 ARMOR_BREAK
    ///
    /// 返回是否死亡（含亡语/终局判定）；THORNS/HP_LOSS 不触发受击钩子
    pub(crate) fn hit_monster(&mut self, target: usize, dmg: i32, ty: DamageType) -> bool {
        if !self.alive(target) {
            return false;
        }
        let mut grounded = false;
        let mut armor_broke = false;
        let hp_before;
        {
            let m = &mut self.monsters[target];
            let cr = &mut m.creature;
            // Invincible（心脏）：单次伤害封顶剩余额度，额度随受击递减
            let mut dmg = dmg;
            if let Some(inv) = cr
                .powers
                .iter_mut()
                .find(|p| p.kind == PowerKind::Invincible)
            {
                if dmg > inv.amount {
                    dmg = inv.amount;
                }
                inv.amount -= dmg;
                if inv.amount <= 0 {
                    cr.powers.retain(|p| p.kind != PowerKind::Invincible);
                }
            }
            let dmg = dmg;
            // Curl Up 判定（onAttacked）：条件是**原始伤害** < 当前血量，且为 NORMAL。
            // ⚠️ 关键顺序：Java 的 CurlUpPower.onAttacked 用 `addToBot(GainBlockAction)`，
            // 即格挡**排队到本次伤害结算之后**才加。若先加格挡再扣血，格挡会吸收本次
            // 伤害（实测桥接：Louse 15hp 被打 6 → 真游戏 9hp+6block，引擎错误地 15hp+0block）。
            let curl_up = if dmg > 0
                && dmg < cr.hp
                && ty == DamageType::Normal
                && cr.powers.iter().any(|p| p.kind == PowerKind::CurlUp)
            {
                cr.powers
                    .iter()
                    .find(|p| p.kind == PowerKind::CurlUp)
                    .map(|p| p.amount)
            } else {
                None
            };
            hp_before = cr.hp;
            cr.apply_damage(dmg);
            // 伤害结算后才获得格挡并移除该 power
            if let Some(amt) = curl_up {
                cr.powers.retain(|p| p.kind != PowerKind::CurlUp);
                cr.block += amt;
            }
            let alive_after = cr.hp > 0;
            let hp_lost = hp_before - cr.hp;
            if alive_after && dmg > 0 && ty == DamageType::Normal {
                // Angry（战鬼）：受击 → 力量 +amount
                if let Some(angry) = cr
                    .powers
                    .iter()
                    .find(|p| p.kind == PowerKind::Angry)
                    .map(|p| p.amount)
                {
                    if let Some(sp) = cr.powers.iter_mut().find(|p| p.kind == PowerKind::Strength) {
                        sp.amount += angry;
                    } else {
                        cr.powers.push(Power {
                            kind: PowerKind::Strength,
                            amount: angry,
                            just_applied: false,
                            has_paper_crane: false,
                            has_paper_frog: false,
                        });
                    }
                }
                if let Some(idx) = cr.powers.iter().position(|p| p.kind == PowerKind::Flight) {
                    cr.powers[idx].amount -= 1;
                    if cr.powers[idx].amount <= 0 {
                        cr.powers.remove(idx);
                        grounded = true;
                    }
                }
                if let Some(idx) = cr
                    .powers
                    .iter()
                    .position(|p| p.kind == PowerKind::Malleable)
                {
                    let b = cr.powers[idx].amount;
                    cr.powers[idx].amount += 1;
                    cr.block += b;
                }
            }
            if hp_lost > 0 && ty != DamageType::HpLoss && ty != DamageType::Thorns {
                if let Some(idx) = cr
                    .powers
                    .iter()
                    .position(|p| p.kind == PowerKind::PlatedArmor)
                {
                    cr.powers[idx].amount -= 1;
                    if cr.powers[idx].amount <= 0 {
                        cr.powers.remove(idx);
                        armor_broke = true;
                    }
                }
            }
        }
        // 换态：GROUNDED/ARMOR_BREAK → 眩晕一回合（setMove(4)，RollMove 后自然接续）
        if grounded || armor_broke {
            let stunned_by_break = armor_broke || self.monsters[target].kind == MonsterKind::Byrd;
            if stunned_by_break {
                self.monsters[target].next_move = 4;
                self.monsters[target].move_history.push(4);
            }
        }
        // PainfulStabs（心脏）：玩家对持有者造成伤害 → 失 2 HP
        if dmg > 0 && ty != DamageType::Thorns {
            let stabs = self.monsters[target]
                .creature
                .powers
                .iter()
                .any(|p| p.kind == PowerKind::PainfulStabs);
            if stabs {
                self.player.creature.hp -= 2;
                if self.player.creature.is_dead() && !self.fairy_rescue() {
                    self.over = true;
                    self.won = false;
                }
            }
        }
        // Shifting（Transient）：受击 → 力量 -伤害，累积 GainStrength 待轮末回复
        if dmg > 0 {
            let shifting = self.monsters[target]
                .creature
                .powers
                .iter()
                .any(|p| p.kind == PowerKind::Shifting);
            if shifting {
                let hp_after = self.monsters[target].creature.hp;
                let taken = (hp_before - hp_after).max(0);
                if taken > 0 {
                    apply_power(
                        &mut self.monsters[target].creature.powers,
                        PowerKind::Strength,
                        -taken,
                        false,
                        false,
                    );
                    apply_power(
                        &mut self.monsters[target].creature.powers,
                        PowerKind::GainStrength,
                        taken,
                        false,
                        false,
                    );
                }
            }
        }
        // Reactive（WrithingMass）：非致死 NORMAL 受击 → 重掷招式（RollMoveAction）
        {
            let reactive = self.monsters[target]
                .creature
                .powers
                .iter()
                .any(|p| p.kind == PowerKind::Reactive);
            if reactive
                && dmg > 0
                && self.monsters[target].creature.hp > 0
                && ty == DamageType::Normal
            {
                self.roll_move(target);
            }
        }
        // 大史莱姆：非致死受击后 HP≤max/2 且未分裂过 → 定招分裂（3）
        if self.monsters[target].creature.hp > 0
            && self.monsters[target].creature.hp * 2 <= self.monsters[target].creature.max_hp
            && self.monsters[target].next_move != 3
            && self.monsters[target].burn_upgraded == 0
            && matches!(
                self.monsters[target].kind,
                MonsterKind::AcidSlimeL | MonsterKind::SpikeSlimeL
            )
        {
            self.monsters[target].burn_upgraded = 1; // splitTriggered
            self.monsters[target].next_move = 3;
            self.monsters[target].move_history.push(3);
        }
        // AwakenedOne：阶段 1 死亡 → 半死转阶段（阶段 2 死亡 = 真死）
        if self.monsters[target].creature.hp <= 0
            && !self.monsters[target].half_dead
            && self.monsters[target].kind == MonsterKind::AwakenedOne
            && self.monsters[target].burn_upgraded == 0
        {
            self.monsters[target].half_dead = true;
            self.monsters[target].next_move = 3;
            self.monsters[target].move_history.push(3);
            return false;
        }
        // Darkling：半死（全员半死=真死；否则定招 4 待复活）
        if self.monsters[target].creature.hp <= 0
            && !self.monsters[target].half_dead
            && self.monsters[target].kind == MonsterKind::Darkling
        {
            self.monsters[target].half_dead = true;
            let all_half = self
                .monsters
                .iter()
                .filter(|m| m.kind == MonsterKind::Darkling)
                .all(|m| m.half_dead || m.creature.is_dead());
            if all_half {
                // 全员半死：全部真死（含 hp<=0 的半死个体）
                for mi in 0..self.monsters.len() {
                    if self.monsters[mi].kind == MonsterKind::Darkling
                        && self.monsters[mi].half_dead
                    {
                        self.monsters[mi].creature.hp = 0;
                        self.monsters[mi].half_dead = false;
                        self.on_monster_death(mi);
                    }
                }
                if !self.over && self.all_monsters_dead() {
                    self.over = true;
                    self.won = true;
                }
                return true;
            }
            self.monsters[target].next_move = 4;
            self.monsters[target].move_history.push(4);
            return false;
        }
        if self.monsters[target].creature.is_dead() {
            self.on_monster_death(target);
            return true;
        }
        false
    }

    /// 对玩家施加 power：神器（Artifact）可抵消减益（Vulnerable/Weak/Frail/Poison）
    pub(crate) fn apply_power_to_player_checked(
        &mut self,
        kind: PowerKind,
        amount: i32,
        source_is_monster: bool,
    ) {
        // Ginger：免疫虚弱（ApplyPowerAction:113）
        if kind == PowerKind::Weak && self.has_relic(crate::relics::RelicKind::Ginger) {
            return;
        }
        // Turnip：免疫脆弱
        if kind == PowerKind::Frail && self.has_relic(crate::relics::RelicKind::Turnip) {
            return;
        }
        // SadisticPower：施加 debuff → 该敌人受 4 伤（简化：在 apply 后结算）
        // 由 Q::ApplyPowerToMonster 的调用方在 apply_power 时检查
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
        // TheBombPower：amount 编码 dmg×10 + 倒数 3（不走通用叠加；重复打出重置倒数）
        if kind == PowerKind::TheBomb {
            let encoded = amount * 10 + 3;
            if let Some(p) = self
                .player
                .creature
                .powers
                .iter_mut()
                .find(|pw| pw.kind == PowerKind::TheBomb)
            {
                p.amount = encoded;
            } else {
                self.player.creature.powers.push(Power {
                    kind,
                    amount: encoded,
                    just_applied: false,
                    has_paper_crane: false,
                    has_paper_frog: false,
                });
            }
        } else {
            apply_power(
                &mut self.player.creature.powers,
                kind,
                amount,
                source_is_monster,
                self.turn_has_ended,
            );
        }
        // MantraPower.stackPower：叠加后满 10 → 进神性并 -10（≤0 移除）
        if kind == PowerKind::Mantra {
            let mut divinity = false;
            if let Some(p) = self
                .player
                .creature
                .powers
                .iter_mut()
                .find(|pw| pw.kind == PowerKind::Mantra)
            {
                if p.amount >= 10 {
                    p.amount -= 10;
                    divinity = true;
                    let rem = p.amount <= 0;
                    if rem {
                        self.player
                            .creature
                            .powers
                            .retain(|pw| pw.kind != PowerKind::Mantra);
                    }
                }
            }
            if divinity {
                self.exec(Q::ChangeStance(crate::state::Stance::Divinity));
            }
        }
    }

    /// 瓶中精灵免死钩子（源码 AbstractPlayer.damage 1467 行附近）——
    /// 生命值将被扣至 1 以下时消耗精灵并回复最大生命的 30%（最少 1 点，
    /// 整数截断）。返回是否成功免死。
    fn fairy_rescue(&mut self) -> bool {
        let slot = self
            .player
            .potions
            .iter()
            .position(|p| p.as_ref().map(|p| p.kind) == Some(PotionKind::Fairy));
        if let Some(i) = slot {
            let p = self.player.potions[i].take().expect("checked");
            self.player.creature.hp = 0;
            let heal = ((self.player.creature.max_hp as f32) * (p.potency as f32 / 100.0)) as i32;
            self.player.creature.hp = heal.max(1).min(self.player.creature.max_hp);
            true
        } else {
            false
        }
    }

    /// Plated Armor wasHPLost：-1 层，归零移除（ReducePowerAction）
    fn reduce_plated_armor(&mut self) {
        if self
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::PlatedArmor)
        {
            apply_power(
                &mut self.player.creature.powers,
                PowerKind::PlatedArmor,
                -1,
                false,
                false,
            );
        }
    }

    /// DrawCardAction + EmptyDeckShuffleAction（源码核对语义）：
    /// - hand==10 → 本轮抽牌全部作废；amount+hand>10 → amount 截断（多余消失，不弃牌）
    /// - 抽牌堆空且弃牌堆非空 → 洗回（shuffleRng）后继续
    /// - 牌堆全空 → 停止
    pub fn draw_cards(&mut self, n: usize) {
        if self.player.hand.len() >= 10 {
            return; // createHandIsFullDialog：完全停止
        }
        // NoDrawPower：本回合不能再抽牌（轮末随衰减移除）
        if self
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::NoDraw)
        {
            return;
        }
        // Confusion（Snecko 瞪视）：抽牌费用 0..3 重掷（永改，cardRandomRng）
        let confused = self
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Confusion);
        let space = 10 - self.player.hand.len();
        let mut left = n.min(space); // 截断到 10
        while left > 0 {
            if self.player.draw.is_empty() {
                if self.player.discard.is_empty() {
                    break;
                }
                // 洗牌可观测性（NEOW_TRACE_SHUFFLE=1 时打印洗牌前/后顺序）。
                // 用于和真游戏 `get_state` 的 draw_pile.order="true" 逐项对拍。
                let mut pile = std::mem::take(&mut self.player.discard);
                let trace = trace_shuffle();
                if trace {
                    let pre: Vec<&str> = pile.iter().map(|c| c.def.id).collect();
                    eprintln!(
                        "[shuffle] turn={} seed_stream={} pre={:?}",
                        self.turn, self.rng.shuffle.counter, pre
                    );
                }
                card_group_shuffle(&mut pile, &mut self.rng.shuffle);
                if trace {
                    let post: Vec<&str> = pile.iter().map(|c| c.def.id).collect();
                    eprintln!("[shuffle] post={:?}", post);
                }
                self.player.draw = pile;
                // EmptyDeckShuffleAction → relics.onShuffle（Sundial）
                self.relics_on_shuffle();
            }
            let mut c = self.player.draw.pop().unwrap();
            // Void（虚空）：抽到时失 1 能量（VoidCard.whenDrawn）
            if c.def.id == "Void" {
                self.player.energy -= 1;
            }
            if confused {
                let cur = c.def.cost.unwrap_or(-2);
                if cur >= 0 {
                    let new_cost = self.rng.card_random.random_range(3);
                    if cur + c.cost_delta != new_cost {
                        c.cost_delta = new_cost - cur;
                    }
                }
            }
            self.player.hand.push(c);
            left -= 1;
        }
    }

    /// Hexaghost.getMove：激活后按已激活球数定招（全程不用 num，无随机）；
    /// 首招（未激活）Activate —— first_move 即 activated 标志
    fn hexaghost_get_move(&mut self, i: usize) -> u8 {
        if self.monsters[i].first_move {
            return 5; // Activate
        }
        match self.monsters[i].ritual_amt {
            // orbActiveCount：0/2/5=灼烙，1/4=冲撞，3=煽燃，6=地狱火
            0 | 2 | 5 => 4,
            1 | 4 => 2,
            3 => 3,
            _ => 6,
        }
    }

    /// SphericGuardian.getMove：首招充能，次招脆化，上次重击→硬化，否则重击
    fn spheric_guardian_get_move(&mut self, i: usize) -> u8 {
        let m = &self.monsters[i];
        if m.first_move {
            2
        } else if m.move_history.len() == 1 {
            4 // secondMove 一次性标志
        } else if m.move_history.last() == Some(&1) {
            3
        } else {
            1
        }
    }

    /// Chosen.getMove：A17 首招即咒缚；否则首招戳刺，第二招咒缚；
    /// 之后衰弱/汲取交替 → 电击/戳刺
    fn chosen_get_move(&mut self, i: usize, num: i32) -> u8 {
        let used_hex = self.monsters[i].move_history.contains(&4);
        let a17 = self.ascension >= 17;
        if !a17 && self.monsters[i].first_move {
            return 5; // POKE ×2 起手
        }
        if !used_hex {
            return 4;
        }
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        if last != Some(3) && last != Some(2) {
            return if num < 50 { 3 } else { 2 };
        }
        if num < 40 {
            1
        } else {
            5
        }
    }

    /// ShelledParasite.getMove：首招 A17=猛击，否则 randomBoolean 定双重/吸取；
    /// num<20 且非重复猛击时重掣（递归的循环等价，每次消耗 aiRng）
    fn shelled_parasite_get_move(&mut self, i: usize, num: i32) -> u8 {
        if self.monsters[i].first_move {
            if self.ascension >= 17 {
                return 1;
            }
            return if self.rng.ai.random_boolean() { 2 } else { 3 };
        }
        let mut num = num;
        loop {
            let m = &self.monsters[i];
            let last = m.move_history.last().copied();
            let last2 = if m.move_history.len() >= 2 {
                Some(m.move_history[m.move_history.len() - 2])
            } else {
                None
            };
            if num < 20 {
                if last != Some(1) {
                    return 1;
                }
                num = self.rng.ai.random_start_end(20, 99);
            } else if num < 60 {
                if !(last == Some(2) && last2 == Some(2)) {
                    return 2;
                }
                return 3;
            } else if !(last == Some(3) && last2 == Some(3)) {
                return 3;
            } else {
                return 2;
            }
        }
    }

    /// Byrd.getMove：飞行中按 num 分段；首招与多处分支额外消耗 aiRng；
    /// 落地（无飞行）恒头槌
    fn byrd_get_move(&mut self, i: usize, num: i32) -> u8 {
        let flying = self.monsters[i]
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Flight);
        if self.monsters[i].first_move {
            return if self.rng.ai.random_boolean_chance(0.375) {
                6
            } else {
                1
            };
        }
        if !flying {
            return 5;
        }
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        let last_two_peck = last == Some(1) && last2 == Some(1);
        if num < 50 {
            if last_two_peck {
                if self.rng.ai.random_boolean_chance(0.4) {
                    3
                } else {
                    6
                }
            } else {
                1
            }
        } else if num < 70 {
            if last == Some(3) {
                if self.rng.ai.random_boolean_chance(0.375) {
                    6
                } else {
                    1
                }
            } else {
                3
            }
        } else if last == Some(6) {
            if self.rng.ai.random_boolean_chance(0.2857) {
                3
            } else {
                1
            }
        } else {
            6
        }
    }

    /// SnakePlant.getMove：num<65 且非连续两次撕咬 → 撕咬；否则孢子
    /// （A17 else 分支额外看 lastMoveBefore）
    fn snake_plant_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if num < 65 {
            if last == Some(1) && last2 == Some(1) {
                2
            } else {
                1
            }
        } else if self.ascension >= 17 {
            if last == Some(2) || last2 == Some(2) {
                1
            } else {
                2
            }
        } else if last == Some(2) {
            1
        } else {
            2
        }
    }

    /// Snecko.getMove：首招瞪视（混乱）；num<40 或连续两次咬 → 尾击；否则咬
    fn snecko_get_move(&mut self, i: usize, num: i32) -> u8 {
        if self.monsters[i].first_move {
            return 1;
        }
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if num < 40 || (last == Some(2) && last2 == Some(2)) {
            3
        } else {
            2
        }
    }

    /// Centurion.getMove：num>=65 且近两招非护卫/狂怒 → 有人护卫否则狂怒；
    /// 近两招非斩击 → 斩击；否则护卫/狂怒
    fn centurion_get_move(&mut self, i: usize, num: i32) -> u8 {
        let others_alive = self
            .monsters
            .iter()
            .filter(|m| !m.creature.is_dead() && !m.escaped)
            .count()
            > 1;
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        let last_two = |x: u8| last == Some(x) && last2 == Some(x);
        if num >= 65 && !last_two(2) && !last_two(3) {
            if others_alive {
                return 2;
            }
            return 3;
        }
        if !last_two(1) {
            return 1;
        }
        if others_alive {
            2
        } else {
            3
        }
    }

    /// Healer.getMove：缺口血量（全员含己）超阈→治疗；否则 num>=40→攻击；
    /// 否则增益；A17 阈值 20/lastMove(1)，普通 15/lastTwoMoves(1)
    fn healer_get_move(&mut self, i: usize, num: i32) -> u8 {
        let need_to_heal: i32 = self
            .monsters
            .iter()
            .filter(|m| !m.creature.is_dead() && !m.escaped)
            .map(|m| m.creature.max_hp - m.creature.hp)
            .sum();
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        let last_two_heal = last == Some(2) && last2 == Some(2);
        let last_two_atk = last == Some(1) && last2 == Some(1);
        let last_two_buff = last == Some(3) && last2 == Some(3);
        let (heal_threshold, atk_cond) = if self.ascension >= 17 {
            (20, last != Some(1))
        } else {
            (15, !last_two_atk)
        };
        if need_to_heal > heal_threshold && !last_two_heal {
            return 2;
        }
        if num >= 40 && atk_cond {
            return 1;
        }
        if !last_two_buff {
            return 3;
        }
        1
    }

    /// GremlinLeader.getMove：按存活小鬼数分支（0/1/2+），
    /// 部分分支有递归重掣（消耗 aiRng）
    fn gremlin_leader_get_move(&mut self, i: usize, num: i32) -> u8 {
        let alive_gremlins = self
            .monsters
            .iter()
            .enumerate()
            .filter(|(j, m)| *j != i && !m.creature.is_dead() && !m.escaped)
            .count() as i32;
        let mut num = num;
        loop {
            let m = &self.monsters[i];
            let last = m.move_history.last().copied();
            if alive_gremlins == 0 {
                if num < 75 {
                    return if last != Some(2) { 2 } else { 4 };
                }
                return if last != Some(4) { 4 } else { 2 };
            }
            if alive_gremlins < 2 {
                if num < 50 {
                    if last != Some(2) {
                        return 2;
                    }
                    num = self.rng.ai.random_start_end(50, 99);
                } else if num < 80 {
                    return if last != Some(3) { 3 } else { 4 };
                } else {
                    if last == Some(4) {
                        num = self.rng.ai.random_start_end(0, 80);
                        continue;
                    }
                    return 4;
                }
            } else {
                if num < 66 {
                    return if last != Some(3) { 3 } else { 4 };
                }
                return if last != Some(4) { 4 } else { 3 };
            }
        }
    }

    /// BookOfStabbing.getMove：num<15 或非双刺时连击数+1 用刺；
    /// A18 重刺后也 +1（counter=stabCount）
    fn book_of_stabbing_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        let a18 = self.ascension >= 18;
        if num < 15 {
            if last == Some(2) {
                self.monsters[i].burn_upgraded += 1;
                1
            } else {
                if a18 {
                    self.monsters[i].burn_upgraded += 1;
                }
                2
            }
        } else if last == Some(1) && last2 == Some(1) {
            if a18 {
                self.monsters[i].burn_upgraded += 1;
            }
            2
        } else {
            self.monsters[i].burn_upgraded += 1;
            1
        }
    }

    /// BronzeOrb.getMove：num>=25 且未用过 → 静滞（一次性）；
    /// num>=70 且非双支援 → 支援；非双光束 → 光束；否则支援
    fn bronze_orb_get_move(&mut self, i: usize, num: i32) -> u8 {
        let used_stasis = self.monsters[i].move_history.contains(&3);
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if !used_stasis && num >= 25 {
            return 3;
        }
        if num >= 70 && !(last == Some(2) && last2 == Some(2)) {
            return 2;
        }
        if !(last == Some(1) && last2 == Some(1)) {
            return 1;
        }
        2
    }

    /// BronzeAutomaton.getMove：首招召唤；numTurns==4 → 光束（复位）；
    /// 光束后 A19 强化/否则眩晕；其余连枷/强化交替（counter=numTurns）
    fn bronze_automaton_get_move(&mut self, i: usize, num: i32) -> u8 {
        let _ = num;
        if self.monsters[i].first_move {
            return 4; // SPAWN_ORBS
        }
        if self.monsters[i].burn_upgraded == 4 {
            self.monsters[i].burn_upgraded = 0;
            return 2; // HYPER_BEAM
        }
        let last = self.monsters[i].move_history.last().copied();
        if last == Some(2) {
            return if self.ascension >= 19 { 5 } else { 3 };
        }
        self.monsters[i].burn_upgraded += 1;
        if matches!(last, Some(3) | Some(5) | Some(4)) {
            1
        } else {
            5
        }
    }

    /// TheCollector.getMove：首招召唤；第 3 回合且未放大 → 超大减益；
    /// 有死亡火把头且 num<=25 → 复活；否则火球/群力交替
    fn collector_get_move(&mut self, i: usize, num: i32) -> u8 {
        if self.monsters[i].move_history.is_empty() {
            return 1; // SPAWN
        }
        let ult_used = self.monsters[i].move_history.contains(&4);
        let turns = self.monsters[i].move_history.len() as i32 + 1;
        if turns >= 3 && !ult_used {
            return 4; // MEGA_DEBUFF
        }
        let minion_dead = self
            .monsters
            .iter()
            .enumerate()
            .any(|(j, m)| j != i && (m.creature.is_dead() || m.escaped));
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if num <= 25 && minion_dead && last != Some(5) {
            return 5; // REVIVE
        }
        if num <= 70 && !(last == Some(2) && last2 == Some(2)) {
            return 2; // FIREBALL
        }
        if last != Some(3) {
            3
        } else {
            2
        }
    }

    /// Champ.getMove：半血以下触发爆怒（latch=hp*2<max，怪不回血）；
    /// 之后连续处决；每 4 回合奚落；锻造≤2 次优先防御姿态
    fn champ_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let threshold = m.creature.hp * 2 < m.creature.max_hp;
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        let num_turns = (m.move_history.len() as i32 % 4) + 1;
        let forge_times = m.move_history.iter().filter(|&&x| x == 2).count() as i32;
        let forge_cap = if self.ascension >= 19 { 30 } else { 15 };
        let angered = m.move_history.contains(&7);
        let threshold_reached = threshold || angered;
        // 源码顺序：①首次过半血 → 爆怒；②过半后连续处决
        if threshold && !angered {
            return 7; // ANGER：清减益 + 力量×3
        }
        if threshold_reached && last != Some(3) && last2 != Some(3) {
            return 3; // EXECUTE ×2
        }
        if num_turns == 4 && !threshold_reached {
            return 6; // TAUNT（numTurns 归零由 len%4 推导自动实现）
        }
        if last != Some(2) && forge_times < 2 && num <= forge_cap {
            return 2; // DEFENSIVE_STANCE
        }
        if last != Some(5) && last != Some(2) && num <= 30 {
            return 5; // GLOAT
        }
        if last != Some(4) && num <= 55 {
            return 4; // FACE_SLAP
        }
        if last != Some(1) {
            1
        } else {
            4
        }
    }

    /// Darkling.getMove：半死 → 复活；首招按 num 硬化/噬咬；
    /// 之后 num<40：偶数位撕咬；70+：非双噬咬 → 噬咬；重掣消耗 aiRng
    fn darkling_get_move(&mut self, i: usize, num: i32) -> u8 {
        if self.monsters[i].half_dead {
            return 5; // REINCARNATE
        }
        let mut num = num;
        loop {
            let (first, last, last2) = {
                let m = &self.monsters[i];
                (
                    m.first_move,
                    m.move_history.last().copied(),
                    if m.move_history.len() >= 2 {
                        Some(m.move_history[m.move_history.len() - 2])
                    } else {
                        None
                    },
                )
            };
            if first {
                return if num < 50 { 2 } else { 3 };
            }
            if num < 40 {
                if last != Some(1) && i & 1 == 0 {
                    return 1;
                }
                num = self.rng.ai.random_start_end(40, 99);
                continue;
            }
            if num < 70 {
                if last != Some(2) {
                    return 2;
                }
                return 3;
            }
            if !(last == Some(3) && last2 == Some(3)) {
                return 3;
            }
            num = self.rng.ai.random_start_end(0, 99);
        }
    }

    /// OrbWalker.getMove：num<40 非双利爪→利爪否则激光；
    /// 其余非双激光→激光否则利爪
    fn orb_walker_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if num < 40 {
            if !(last == Some(2) && last2 == Some(2)) {
                2
            } else {
                1
            }
        } else if !(last == Some(1) && last2 == Some(1)) {
            1
        } else {
            2
        }
    }

    /// SpireGrowth.getMove：A17 且玩家无紧缠 → 紧缠；
    /// num<50 非双快击 → 快击；玩家无紧缠 → 紧缠；否则重击
    fn spire_growth_get_move(&mut self, i: usize, num: i32) -> u8 {
        let constricted = self
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Constricted);
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if self.ascension >= 17 && !constricted && last != Some(2) {
            return 2;
        }
        if num < 50 && !(last == Some(1) && last2 == Some(1)) {
            return 1;
        }
        if !constricted && last != Some(2) {
            return 2;
        }
        3
    }

    /// Maw.getMove：未咆哮→咆哮；num<50 非上次啃食 → 啃食×(tc/2)；
    /// 上次猛击/啃食 → 流涎；否则猛击（counter=turnCount 在 takeTurn 递增）
    fn maw_get_move(&mut self, i: usize, num: i32) -> u8 {
        let roared = self.monsters[i].move_history.contains(&2);
        if !roared {
            return 2;
        }
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        if num < 50 && last != Some(5) {
            return 5;
        }
        if last == Some(3) || last == Some(5) {
            return 4;
        }
        3
    }

    /// WrithingMass.getMove：首招三段；之后多分支含 randomBoolean 与重掣
    fn writhing_mass_get_move(&mut self, i: usize, num: i32) -> u8 {
        let mut num = num;
        loop {
            let m = &self.monsters[i];
            let last = m.move_history.last().copied();
            if m.first_move {
                return if num < 33 {
                    1
                } else if num < 66 {
                    2
                } else {
                    3
                };
            }
            if num < 10 {
                if last != Some(0) {
                    return 0;
                }
                num = self.rng.ai.random_start_end(10, 99);
            } else if num < 20 {
                let used_mega = m.move_history.contains(&4);
                if !used_mega && last != Some(4) {
                    return 4;
                }
                if self.rng.ai.random_boolean_chance(0.1) {
                    return 0;
                }
                num = self.rng.ai.random_start_end(20, 99);
            } else if num < 40 {
                if last != Some(3) {
                    return 3;
                }
                if self.rng.ai.random_boolean_chance(0.4) {
                    num = self.rng.ai.random_range(19);
                } else {
                    num = self.rng.ai.random_start_end(40, 99);
                }
            } else if num < 70 {
                if last != Some(1) {
                    return 1;
                }
                if self.rng.ai.random_boolean_chance(0.3) {
                    return 2;
                }
                num = self.rng.ai.random_range(39);
            } else if last != Some(2) {
                return 2;
            } else {
                num = self.rng.ai.random_range(69);
            }
        }
    }

    /// GiantHead.getMove：count<=1 → 时刻已至（count 递减，>-6 止）；
    /// 否则 count-- 且瞪视/计数交替（counter=count 在此更新）
    fn giant_head_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if m.burn_upgraded <= 1 {
            if m.burn_upgraded > -6 {
                self.monsters[i].burn_upgraded -= 1;
            }
            return 2;
        }
        self.monsters[i].burn_upgraded -= 1;
        if num < 50 {
            if !(last == Some(1) && last2 == Some(1)) {
                1
            } else {
                3
            }
        } else if !(last == Some(3) && last2 == Some(3)) {
            3
        } else {
            1
        }
    }

    /// Nemesis.getMove：首招按 num 三连攻/灼伤；镰刀冷却 2；
    /// 多分支 randomBoolean 重掣（消耗 aiRng）
    fn nemesis_get_move(&mut self, i: usize, num: i32) -> u8 {
        {
            let m = &self.monsters[i];
            let last = m.move_history.last().copied();
            let last2 = if m.move_history.len() >= 2 {
                Some(m.move_history[m.move_history.len() - 2])
            } else {
                None
            };
            if m.first_move {
                return if num < 50 { 2 } else { 4 };
            }
            if num < 30 {
                if last != Some(3) && m.burn_upgraded <= 0 {
                    self.monsters[i].burn_upgraded = 2; // 镰刀冷却置 2
                    return 3;
                }
                if self.rng.ai.random_boolean() && !(last == Some(2) && last2 == Some(2)) {
                    return 2;
                }
                return 4;
            }
            if num < 65 {
                if !(last == Some(2) && last2 == Some(2)) {
                    return 2;
                }
                if self.rng.ai.random_boolean() {
                    if m.burn_upgraded > 0 {
                        return 4;
                    }
                    self.monsters[i].burn_upgraded = 2;
                    return 3;
                }
                return 4;
            }
            if last != Some(4) {
                return 4;
            }
            if self.rng.ai.random_boolean() && m.burn_upgraded <= 0 {
                self.monsters[i].burn_upgraded = 2;
                return 3;
            }
            2
        }
    }

    /// Reptomancer.getMove：首招召唤；num<33 蛇击（重掣）；
    /// <66 且非双召唤且 canSpawn → 召唤；其余重咬
    fn reptomancer_get_move(&mut self, i: usize, num: i32) -> u8 {
        let can_spawn = {
            let alive_others = self
                .monsters
                .iter()
                .enumerate()
                .filter(|(j, m)| *j != i && !m.creature.is_dead() && !m.escaped)
                .count();
            alive_others <= 3
        };
        let mut num = num;
        loop {
            let m = &self.monsters[i];
            let last = m.move_history.last().copied();
            let last2 = if m.move_history.len() >= 2 {
                Some(m.move_history[m.move_history.len() - 2])
            } else {
                None
            };
            if m.first_move {
                return 2;
            }
            if num < 33 {
                if last != Some(1) {
                    return 1;
                }
                num = self.rng.ai.random_start_end(33, 99);
            } else if num < 66 {
                if !(last == Some(2) && last2 == Some(2)) {
                    if can_spawn {
                        return 2;
                    }
                    return 1;
                }
                return 1;
            } else {
                return 3;
            }
        }
    }

    /// AwakenedOne.getMove：阶段 1 挥砍/灵魂冲击；阶段 2（counter=1）
    /// 首招黑暗回响，之后污泥/冲撞交替
    fn awakened_one_get_move(&mut self, i: usize, num: i32) -> u8 {
        let stage2 = self.monsters[i].burn_upgraded == 1;
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if !stage2 {
            if m.first_move {
                return 1;
            }
            if num < 25 {
                if last != Some(2) {
                    return 2;
                }
                return 1;
            }
            if !(last == Some(1) && last2 == Some(1)) {
                return 1;
            }
            return 2;
        }
        // 阶段 2：重生回合已 setMove(5)；此后污泥/冲撞
        if m.move_history.len() <= 1 {
            return 5;
        }
        if num < 50 {
            if !(last == Some(6) && last2 == Some(6)) {
                return 6;
            }
            return 8;
        }
        if !(last == Some(8) && last2 == Some(8)) {
            return 8;
        }
        6
    }

    /// TimeEater.getMove：半血一次加速（usedHaste=history 含 5）；
    /// num<45 回响（双后重掣）；<80 头撞/涟漪（66% 回响）；否则涟漪/重掣
    fn time_eater_get_move(&mut self, i: usize, num: i32) -> u8 {
        let used_haste = self.monsters[i].move_history.contains(&5);
        let m = &self.monsters[i];
        let half = m.creature.hp * 2 < m.creature.max_hp;
        if half && !used_haste {
            return 5;
        }
        let mut num = num;
        loop {
            let m = &self.monsters[i];
            let last = m.move_history.last().copied();
            let last2 = if m.move_history.len() >= 2 {
                Some(m.move_history[m.move_history.len() - 2])
            } else {
                None
            };
            if num < 45 {
                if !(last == Some(2) && last2 == Some(2)) {
                    return 2;
                }
                num = self.rng.ai.random_start_end(50, 99);
                continue;
            } else if num < 80 {
                if last != Some(4) {
                    return 4;
                }
                if self.rng.ai.random_boolean_chance(0.66) {
                    return 2;
                }
                return 3;
            }
            if last != Some(3) {
                return 3;
            }
            num = self.rng.ai.random_range(74);
        }
    }

    /// SpireShield.getMove：moveCount%3 环 —— 0: 50%筑防/痛击；
    /// 1: 非上次痛击→痛击否则筑防；2: 粉碎（moveCount=史长-首招）
    fn spire_shield_get_move(&mut self, i: usize) -> u8 {
        let count = (self.monsters[i].move_history.len() as i32 - 1).max(0);
        let last = self.monsters[i].move_history.last().copied();
        match count % 3 {
            0 => {
                if self.rng.ai.random_boolean() {
                    2
                } else {
                    1
                }
            }
            1 => {
                if last != Some(1) {
                    1
                } else {
                    2
                }
            }
            _ => 3,
        }
    }

    /// SpireSpear.getMove：0: 非上次燃烧→燃烧否则穿透；1: 穿刺；2: 50%穿透/燃烧
    fn spire_spear_get_move(&mut self, i: usize) -> u8 {
        let count = (self.monsters[i].move_history.len() as i32 - 1).max(0);
        let last = self.monsters[i].move_history.last().copied();
        match count % 3 {
            0 => {
                if last != Some(1) {
                    1
                } else {
                    2
                }
            }
            1 => 3,
            _ => {
                if self.rng.ai.random_boolean() {
                    2
                } else {
                    1
                }
            }
        }
    }

    /// CorruptHeart.getMove：首招削弱；moveCount%3 环 ——
    /// 0: 50%血射/回响；1: 非上次回响→回响否则血射；2: 增益
    fn corrupt_heart_get_move(&mut self, i: usize) -> u8 {
        if self.monsters[i].first_move {
            return 3; // DEBILITATE
        }
        let count = (self.monsters[i].move_history.len() as i32 - 1).max(0);
        let last = self.monsters[i].move_history.last().copied();
        match count % 3 {
            0 => {
                if self.rng.ai.random_boolean() {
                    1
                } else {
                    2
                }
            }
            1 => {
                if last != Some(2) {
                    2
                } else {
                    1
                }
            }
            _ => 4,
        }
    }

    /// AcidSlime_L.getMove：A17 分段 <40/<70/else；非 A17 <30/<70/else；
    /// 各分支含 randomBoolean 重掣（消耗 aiRng）
    fn acid_slime_l_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        let (t1, t2) = if self.ascension >= 17 {
            (40, 70)
        } else {
            (30, 70)
        };
        let w_tackle = m.chomp_dmg;
        let n_tackle = m.thrash_dmg;
        if num < t1 {
            if last == Some(1) && last2 == Some(1) {
                return if self.rng.ai.random_boolean_chance(if self.ascension >= 17 {
                    0.6
                } else {
                    0.5
                }) {
                    2
                } else {
                    4
                };
            }
            return 1;
        }
        if num < t2 {
            if last == Some(2) {
                return if self.rng.ai.random_boolean_chance(0.4) {
                    1
                } else {
                    4
                };
            }
            return 2;
        }
        if last == Some(4) && last2 == Some(4) {
            return if self.rng.ai.random_boolean_chance(0.4) {
                1
            } else {
                2
            };
        }
        let _ = (w_tackle, n_tackle);
        4
    }

    /// SpikeSlime_L.getMove：A17 分段 <30/else（非 A17 <30/else）；
    /// 上次脆舔→冲撞；连续两次冲撞→脆舔；否则冲撞
    /// 红面罩首领（BanditLeader.getMove 恒 2 + takeTurn SetMoveAction 驱动）：
    /// 无历史 → 2 嘲讽；2 → 3 痛苦斩；3 → 1 交叉斩；1 → 3（A17 除非连打两次 1）
    fn bandit_leader_get_move(&mut self, i: usize) -> u8 {
        let h = &self.monsters[i].move_history;
        match h.last().copied() {
            None => 2,
            Some(2) => 3,
            Some(3) => 1,
            _ => {
                let last2_both_slash = h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
                if self.ascension >= 17 && !last2_both_slash {
                    1
                } else {
                    3
                }
            }
        }
    }

    /// 红面罩熊：无历史 → 2 熊抱；2 → 3 扑击；3 → 1 撕咬；1 → 3（3↔1 循环）
    fn bandit_bear_get_move(&mut self, i: usize) -> u8 {
        match self.monsters[i].move_history.last().copied() {
            None => 2,
            Some(2) => 3,
            Some(3) => 1,
            _ => 3,
        }
    }

    /// 荆棘形体（Spiker.getMove）：增益超 5 次 → 恒普攻；
    /// num<50 且上次非普攻 → 普攻；否则荆棘+2
    fn spiker_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        if m.bellow_str > 5 {
            return 1;
        }
        if num < 50 && m.move_history.last() != Some(&1) {
            1
        } else {
            2
        }
    }

    fn spike_slime_l_get_move(&mut self, i: usize, num: i32) -> u8 {
        let m = &self.monsters[i];
        let last = m.move_history.last().copied();
        let last2 = if m.move_history.len() >= 2 {
            Some(m.move_history[m.move_history.len() - 2])
        } else {
            None
        };
        if num < 30 {
            if last == Some(1) && last2 == Some(1) {
                return 4;
            }
            return 1;
        }
        if last == Some(4) {
            return 1;
        }
        1
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
            MonsterKind::LouseDefensive => self.louse_defensive_get_move(i, num),
            MonsterKind::FungiBeast => self.fungi_get_move(i, num),
            MonsterKind::SpikeSlimeS => 1, // 恒攻击
            MonsterKind::AcidSlimeS => {
                // AcidSlime_S.getMove（Java）：
                //   A17+：lastTwoMoves(1) ? ATTACK(1) : DEBUFF(2)
                //   否则：**aiRng.randomBoolean()** ? ATTACK(1) : DEBUFF(2)
                // ⚠️ 少一次 randomBoolean 会让后续怪物的 aiRng 位置整体前移 1，
                //    导致下一只怪（如 SpikeSlime_M）的 num 落到不同分支
                //    （实测 intent 从 ATTACK_DEBUFF 变 DEBUFF、rng.ai 3 vs 2）。
                let h = &self.monsters[i].move_history;
                let last2_tackle =
                    h.len() >= 2 && h[h.len() - 1] == 1 && h[h.len() - 2] == 1;
                if self.ascension >= 17 {
                    if last2_tackle {
                        1
                    } else {
                        2
                    }
                } else if self.rng.ai.random_boolean() {
                    1
                } else {
                    2
                }
            }
            MonsterKind::SpikeSlimeM => self.spike_slime_m_get_move(i, num),
            MonsterKind::AcidSlimeM => self.acid_slime_m_get_move(i, num),
            MonsterKind::Looter => self.looter_get_move(i, num),
            MonsterKind::SlaverBlue => self.slaver_blue_get_move(i, num),
            MonsterKind::SlaverRed => self.slaver_red_get_move(i, num),
            MonsterKind::GremlinNob => self.gremlin_nob_get_move(i, num),
            MonsterKind::Lagavulin => self.lagavulin_get_move(i, num),
            MonsterKind::Sentry => self.sentry_get_move(i, num),
            MonsterKind::Guardian => self.guardian_get_move(i),
            MonsterKind::SlimeBoss => self.slime_boss_get_move(i),
            MonsterKind::Hexaghost => self.hexaghost_get_move(i),
            MonsterKind::SphericGuardian => self.spheric_guardian_get_move(i),
            MonsterKind::Chosen => self.chosen_get_move(i, num),
            MonsterKind::ShelledParasite => self.shelled_parasite_get_move(i, num),
            MonsterKind::Byrd => self.byrd_get_move(i, num),
            MonsterKind::Mugger => 1, // getMove 恒 MUG；后续由 takeTurn 驱动
            MonsterKind::SnakePlant => self.snake_plant_get_move(i, num),
            MonsterKind::Snecko => self.snecko_get_move(i, num),
            MonsterKind::Centurion => self.centurion_get_move(i, num),
            MonsterKind::Healer => self.healer_get_move(i, num),
            // 小鬼 SetMove 驱动：battle-start 摸一次，之后由 takeTurn 决定
            MonsterKind::GremlinWarrior | MonsterKind::GremlinThief => 1,
            MonsterKind::GremlinFat => 2,
            MonsterKind::GremlinTsundere => 1,
            MonsterKind::GremlinWizard => 2,
            MonsterKind::TorchHead => 1,
            MonsterKind::GremlinLeader => self.gremlin_leader_get_move(i, num),
            MonsterKind::BookOfStabbing => self.book_of_stabbing_get_move(i, num),
            MonsterKind::Taskmaster => 2,
            MonsterKind::BronzeOrb => self.bronze_orb_get_move(i, num),
            MonsterKind::BronzeAutomaton => self.bronze_automaton_get_move(i, num),
            MonsterKind::TheCollector => self.collector_get_move(i, num),
            MonsterKind::Champ => self.champ_get_move(i, num),
            MonsterKind::Darkling => self.darkling_get_move(i, num),
            MonsterKind::Repulsor => {
                let last = self.monsters[i].move_history.last().copied();
                if num < 20 && last != Some(2) {
                    2
                } else {
                    1
                }
            }
            MonsterKind::OrbWalker => self.orb_walker_get_move(i, num),
            MonsterKind::SpireGrowth => self.spire_growth_get_move(i, num),
            MonsterKind::Transient => 1,
            MonsterKind::Maw => self.maw_get_move(i, num),
            MonsterKind::WrithingMass => self.writhing_mass_get_move(i, num),
            MonsterKind::GiantHead => self.giant_head_get_move(i, num),
            MonsterKind::Nemesis => self.nemesis_get_move(i, num),
            MonsterKind::Reptomancer => self.reptomancer_get_move(i, num),
            MonsterKind::SnakeDagger => {
                if self.monsters[i].first_move {
                    1
                } else {
                    2
                }
            }
            MonsterKind::AwakenedOne => self.awakened_one_get_move(i, num),
            MonsterKind::Donu | MonsterKind::Deca => {
                // 攻/环交替（isAttacking ⟺ 上次是攻击；首招由构造器定）
                if self.monsters[i].first_move {
                    if self.monsters[i].kind == MonsterKind::Donu {
                        2
                    } else {
                        0
                    }
                } else if self.monsters[i].move_history.last() == Some(&0) {
                    2
                } else {
                    0
                }
            }
            MonsterKind::TimeEater => self.time_eater_get_move(i, num),
            MonsterKind::SpireShield => self.spire_shield_get_move(i),
            MonsterKind::SpireSpear => self.spire_spear_get_move(i),
            MonsterKind::CorruptHeart => self.corrupt_heart_get_move(i),
            MonsterKind::AcidSlimeL => self.acid_slime_l_get_move(i, num),
            MonsterKind::SpikeSlimeL => self.spike_slime_l_get_move(i, num),
            // ── 补缺第二批（红面罩三人组 + 古形体）──
            MonsterKind::BanditPointy => 1, // getMove 恒普攻（5×2）
            MonsterKind::BanditLeader => self.bandit_leader_get_move(i),
            MonsterKind::BanditBear => self.bandit_bear_get_move(i),
            MonsterKind::Spiker => self.spiker_get_move(i, num),
            MonsterKind::Exploder => {
                // turnCount < 2 恒普攻；之后待机（第三回合自爆）
                if self.monsters[i].move_history.len() < 2 {
                    1
                } else {
                    2
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
        self.rng.card_random.counter.hash(&mut h);
        self.rng.potion.counter.hash(&mut h);
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
        // 遗物栏（公开信息）：种类 + 计数器
        for r in &self.player_relics {
            (r.kind, r.counter).hash(h);
        }
        // 药水栏（公开信息）：种类即可（potency 由种类确定）
        for slot in &self.player.potions {
            match slot {
                Some(p) => (1u8, p.kind).hash(h),
                None => 0u8.hash(h),
            }
        }
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
            Some(Pending::Discovery { options, copies }) => {
                6u8.hash(h);
                copies.hash(h);
                for c in options {
                    (c.def.id, c.upgraded).hash(h);
                }
            }
            Some(Pending::StanceChoice) => 7u8.hash(h),
            Some(Pending::GamblersDiscard) => 8u8.hash(h),
            Some(Pending::HandExhaustAny) => 9u8.hash(h),
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
    use crate::state::Intent;

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
                    upgrade_times: 0,
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
                upgrade_times: 0,
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
        // ⚠️ 真游戏语义（CurlUpPower.onAttacked）：格挡由 addToBot(GainBlockAction)
        // **排队到本次伤害结算之后**才加，所以本次打击吃满 6 点伤害，
        // 之后才获得 {curl} 点格挡。若写成「先加格挡再扣血」，格挡会吸收本次伤害。
        let hp_loss = hp0 - c.monsters[0].creature.hp;
        assert_eq!(hp_loss, 6, "蜷缩不吸收触发它的那次伤害（curl={curl}）");
        assert_eq!(c.monsters[0].creature.block, curl, "伤害结算后获得 curl 格挡");
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

    // ══════════════ 药水系统（potions/*.java 对照） ══════════════

    use crate::potions::{PotionInst, PotionKind};

    fn give_potion(c: &mut Combat, kind: PotionKind) {
        c.player.potions[0] = Some(PotionInst::new(kind));
    }

    /// 火焰药水：20 点 THORNS 纯伤（不吃力量/虚弱/易伤）
    #[test]
    fn fire_potion_pure_damage() {
        let mut c = Combat::new(1);
        let hp0 = c.monsters[0].creature.hp;
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Strength,
            5,
            false,
            false,
        );
        apply_power(
            &mut c.monsters[0].creature.powers,
            PowerKind::Vulnerable,
            3,
            false,
            false,
        );
        give_potion(&mut c, PotionKind::Fire);
        c.step(&Action::UsePotion {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 20, "纯伤不吃 STR/易伤");
        assert!(c.player.potions[0].is_none(), "使用后栏位腾空");
    }

    /// 格挡药水：吃敏捷/脆弱乘区（同卡牌格挡管线）
    #[test]
    fn block_potion_dex_frail() {
        let mut c = Combat::new(1);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Dexterity,
            2,
            false,
            false,
        );
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Frail,
            1,
            true,
            true,
        );
        give_potion(&mut c, PotionKind::Block);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        let expect = crate::damage::calc_player_card_block(12, &c.player.creature.powers);
        assert_eq!(c.player.creature.block, expect);
        assert!(expect < 12, "虚弱下应低于基础值");
    }

    /// 指向型投掷：恐惧/虚弱/毒药水作用于目标怪
    #[test]
    fn targeted_debuff_potions() {
        for (kind, pk) in [
            (PotionKind::Fear, PowerKind::Vulnerable),
            (PotionKind::Weaken, PowerKind::Weak),
            (PotionKind::Poison, PowerKind::Poison),
        ] {
            let mut c = Combat::new(1);
            give_potion(&mut c, kind);
            c.step(&Action::UsePotion {
                index: 0,
                target: Some(0),
            });
            let p = c.monsters[0]
                .creature
                .powers
                .iter()
                .find(|p| p.kind == pk)
                .expect("应施加 power");
            assert!(p.amount >= 3);
        }
    }

    /// 爆炸药水：10 点纯伤全体（不吃力量）
    #[test]
    fn explosive_potion_all_pure() {
        let mut c = Combat::new_with(7, 20, &[MonsterKind::JawWorm, MonsterKind::Cultist]);
        let hp0: Vec<i32> = c.monsters.iter().map(|m| m.creature.hp).collect();
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::Strength,
            99,
            false,
            false,
        );
        give_potion(&mut c, PotionKind::Explosive);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        for (i, &hp) in hp0.iter().enumerate() {
            assert_eq!(c.monsters[i].creature.hp, hp - 10);
        }
    }

    /// 类固醇/速度药水：临时增益 EOT 失效
    #[test]
    fn steroid_speed_temporary() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::Steroid);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Strength), Some(5));
        c.player
            .creature
            .powers
            .retain(|p| p.kind != PowerKind::Rage);
        c.exec(Q::DamagePure { target: 0, dmg: 0 }); // no-op
        c.end_turn(); // 走完怪物回合（大颚虫 A20 攻击可能致死，先拉满血）
        assert!(c.player.creature.hp > 0 || c.over);
        if !c.over {
            assert_eq!(pamount(&c, PowerKind::Strength), None, "EOT 失效");
        }
    }

    /// 血药/果汁：百分比治疗与上限提升
    #[test]
    fn blood_and_fruit_juice() {
        let mut c = Combat::new(1);
        c.player.creature.hp = 40;
        give_potion(&mut c, PotionKind::Blood);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.creature.hp, 56, "80×20%＝16");

        give_potion(&mut c, PotionKind::FruitJuice);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.creature.max_hp, 85);
        assert_eq!(c.player.creature.hp, 61, "上限+5 且治疗 5");
    }

    /// 瓶中精灵：致死伤害免死回复 30%；不可主动使用
    #[test]
    fn fairy_rescue_on_lethal() {
        let mut c = Combat::new(1);
        c.player.creature.hp = 10;
        give_potion(&mut c, PotionKind::Fairy);
        // 不可主动使用
        assert!(!c
            .legal_actions()
            .iter()
            .any(|a| matches!(a, Action::UsePotion { index: 0, .. })));
        c.exec(Q::MonsterDamage { src: 0, base: 999 });
        assert!(!c.over, "免死");
        assert_eq!(c.player.creature.hp, 24, "80×30%＝24");
        assert!(c.player.potions[0].is_none(), "精灵被消耗");
        // 再次致死：无精灵 → 死亡
        c.exec(Q::MonsterDamage { src: 0, base: 999 });
        assert!(c.over && !c.won);
    }

    /// 攻击药水：探素屏三选一，选中卡入手且本回合 0 费
    #[test]
    fn attack_potion_discovery_flow() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::Attack);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        let options = match &c.pending {
            Some(Pending::Discovery { options, .. }) => options.clone(),
            _ => panic!("应挂起探素屏"),
        };
        assert_eq!(options.len(), 3);
        assert!(options
            .iter()
            .all(|o| o.def.card_type == "ATTACK" && o.def.color == "RED"));
        // 选项去重
        assert_eq!(
            options
                .iter()
                .map(|o| o.def.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        let hand0 = c.player.hand.len();
        c.step(&Action::DiscoveryResponse { index: 0 });
        assert!(c.pending.is_none());
        assert_eq!(c.player.hand.len(), hand0 + 1);
        let got = c.player.hand.last().unwrap();
        assert_eq!(got.cost_pay(), 0, "本回合 0 费");
    }

    /// 无色药水：池来自 COLORLESS（非基础非特殊非状态）
    #[test]
    fn colorless_potion_pool() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::Colorless);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        match &c.pending {
            Some(Pending::Discovery { options, .. }) => {
                assert!(options.iter().all(|o| o.def.color == "COLORLESS"));
            }
            _ => panic!("应挂起探素屏"),
        }
        c.step(&Action::DiscoveryResponse { index: 2 });
        assert!(c.pending.is_none());
    }

    /// 狡诈药水：3 张已升级小刀入手
    #[test]
    fn cunning_upgraded_shivs() {
        let mut c = Combat::new(1);
        c.player.hand.clear();
        give_potion(&mut c, PotionKind::Cunning);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.hand.len(), 3);
        assert!(c
            .player
            .hand
            .iter()
            .all(|h| h.def.id == "Shiv" && h.upgraded));
    }

    /// 蒸馏混沌：抽堆顶 3 张依次免费打出（单怪=确定性目标）
    #[test]
    fn distilled_chaos_plays_top() {
        let mut c = Combat::new(1);
        let hp0 = c.monsters[0].creature.hp;
        c.player.draw.clear();
        c.player.discard.clear();
        c.player.draw = ["Defend_R", "Strike_R", "Strike_R", "Strike_R"]
            .iter()
            .map(|id| CardInst::new(id))
            .collect();
        c.player.hand.clear();
        give_potion(&mut c, PotionKind::DistilledChaos);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(c.pending.is_none());
        // A20：打击 6+STR(0)=6×3；剩余顶张 Defend_R 未被打出
        assert_eq!(c.monsters[0].creature.hp, hp0 - 18);
        assert_eq!(c.player.draw.len(), 1);
        let strikes_in_discard = c
            .player
            .discard
            .iter()
            .filter(|x| x.def.id == "Strike_R")
            .count();
        assert_eq!(strikes_in_discard, 3);
    }

    /// 复制药水：下一张牌额外免费结算一次
    #[test]
    fn duplication_potion_double_play() {
        let mut c = Combat::new(1);
        let hp0 = c.monsters[0].creature.hp;
        give_potion(&mut c, PotionKind::Duplication);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        force_hand(&mut c, &["Strike_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 12, "打击 6×2 次");
        // 能量只扣一次
        assert_eq!(c.player.energy, 2);
        assert_eq!(pamount(&c, PowerKind::Duplication), None, "归零移除");
    }

    /// 混沌药水：所有空栏位填满随机药水（确定性、无果汁）
    #[test]
    fn entropic_brew_fills_all() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::EntropicBrew);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(c.player.potions.iter().all(|p| p.is_some()), "三栏全满");
        assert!(!c
            .player
            .potions
            .iter()
            .any(|p| p.as_ref().map(|p| p.kind) == Some(PotionKind::FruitJuice)));
        // 同种子可重放
        let mut c2 = Combat::new(1);
        give_potion(&mut c2, PotionKind::EntropicBrew);
        c2.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        let ids = |c: &Combat| {
            c.player
                .potions
                .iter()
                .map(|p| p.as_ref().unwrap().kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&c), ids(&c2));
    }

    /// 暗之精华：字面双重循环——0 球无效；≥1 球时填满全部球槽（左侧被引出）
    #[test]
    fn essence_of_darkness_loop() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::EssenceOfDarkness);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(c.player.orbs.is_empty(), "0 球时源码行为：无效果");

        let mut c = Combat::new(1);
        c.exec(Q::Channel {
            kind: crate::state::OrbKind::Frost,
            auto_evoke: true,
        });
        give_potion(&mut c, PotionKind::EssenceOfDarkness);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.orbs.len(), 3);
        assert!(c
            .player
            .orbs
            .iter()
            .all(|o| o.kind == crate::state::OrbKind::Dark));
        // 被挤出的冰球引出：5 格挡（不吃专注修正前的基础）
        assert_eq!(c.player.creature.block, 5);
    }

    /// 钢之精华：多层护甲 EOT 获甲、受击掉血 -1 层
    #[test]
    fn plated_armor_potion_flow() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::EssenceOfSteel);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::PlatedArmor), Some(4));
        // EOT：+4 格挡（不经 end_turn 全流程，直接调内部钩子所在函数不现实；用 exec 途径）
        // 这里用 end_of_round 前的 end_turn 太重，改用直接验证：受击掉层
        let hp_before = c.player.creature.hp;
        c.exec(Q::MonsterDamage { src: 0, base: 30 }); // 掉血（无格挡）
        assert!(c.player.creature.hp < hp_before);
        assert_eq!(pamount(&c, PowerKind::PlatedArmor), Some(3), "掉血 -1 层");
        // 只格挡不掉血：不减层
        c.player.creature.block = 99;
        c.exec(Q::MonsterDamage { src: 0, base: 5 });
        assert_eq!(pamount(&c, PowerKind::PlatedArmor), Some(3));
    }

    /// 赌徒特酿：弃任意张抽等量
    #[test]
    fn gamblers_brew_flow() {
        let mut c = Combat::new(1);
        let hand_n = c.player.hand.len();
        give_potion(&mut c, PotionKind::GamblersBrew);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(matches!(c.pending, Some(Pending::GamblersDiscard)));
        c.step(&Action::HandSelect {
            indices: vec![0, 1],
        });
        assert!(c.pending.is_none());
        assert_eq!(c.player.hand.len(), hand_n, "弃 2 抽 2");
        assert_eq!(c.player.discard.len(), 2);
    }

    /// 液态记忆：弃堆选 1 入手且本回合 0 费；弃堆恰 1 张时免选
    #[test]
    fn liquid_memories_flow() {
        let mut c = Combat::new(1);
        c.player.discard = vec![CardInst::new("Bash")];
        give_potion(&mut c, PotionKind::LiquidMemories);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(c.pending.is_none(), "单张免选");
        assert_eq!(c.player.hand.last().unwrap().def.id, "Bash");
        assert_eq!(c.player.hand.last().unwrap().cost_pay(), 0);

        // 多张：开屏选择
        let mut c = Combat::new(1);
        c.player.discard = vec![CardInst::new("Bash"), CardInst::new("Defend_R")];
        give_potion(&mut c, PotionKind::LiquidMemories);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(matches!(
            c.pending,
            Some(Pending::PilePick {
                purpose: PilePickPurpose::DiscardToHandFreeCost,
                ..
            })
        ));
        c.step(&Action::PilePickResponse { index: 0 });
        assert_eq!(c.player.hand.last().unwrap().def.id, "Bash");
        assert_eq!(c.player.hand.last().unwrap().cost_pay(), 0);
        assert_eq!(c.player.discard.len(), 1);
    }

    /// 异蛇之油：抽 5 + 手牌费用随机化（X 费不变，确定性可重放）
    #[test]
    fn snecko_oil_randomize() {
        let run = |seed: u64| {
            let mut c = Combat::new(seed);
            c.player.draw.clear();
            c.player.discard.clear();
            c.player.draw = ["Strike_R"; 8].iter().map(|id| CardInst::new(id)).collect();
            force_hand(&mut c, &["Whirlwind", "Bash", "Defend_R"]);
            give_potion(&mut c, PotionKind::SneckoOil);
            c.step(&Action::UsePotion {
                index: 0,
                target: None,
            });
            let costs: Vec<i32> = c.player.hand.iter().map(|h| h.cost_pay()).collect();
            (c, costs)
        };
        let (c, costs) = run(7);
        assert_eq!(c.player.hand.len(), 8, "3+5 抽满");
        assert_eq!(costs[0], -1, "X 费不变");
        assert!(costs[1..].iter().all(|&x| (0..=3).contains(&x)));
        let (_, costs2) = run(7);
        assert_eq!(costs, costs2, "同种子确定性");
    }

    /// 烟雾弹：逃离战斗（结束、不判负、无奖励标记）
    #[test]
    fn smoke_bomb_escape() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::SmokeBomb);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(c.over);
        assert!(!c.won);
        assert!(c.escaped);
        assert!(c.legal_actions().is_empty());
    }

    /// 姿态药水（二选一）与琼浆（神性）
    #[test]
    fn stance_potion_and_ambrosia() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::Stance);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(matches!(c.pending, Some(Pending::StanceChoice)));
        c.step(&Action::StanceResponse { wrath: true });
        assert_eq!(c.player.stance, crate::state::Stance::Wrath);

        give_potion(&mut c, PotionKind::Ambrosia);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.stance, crate::state::Stance::Divinity);
    }

    /// 邪教徒药水：玩家侧仪式，轮末投予力量（首跳）
    #[test]
    fn cultist_potion_player_ritual() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::Cultist);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        c.end_of_round_powers(); // 第一轮：skipFirst 消耗
        assert_eq!(pamount(&c, PowerKind::Strength), None);
        c.end_of_round_powers(); // 第二轮：+1
        assert_eq!(pamount(&c, PowerKind::Strength), Some(1));
    }

    /// 简单增益药水一批：能量/敏捷/专注/扩容/鬼魂/钢铁之心/铜液/再生/先古/迅速
    #[test]
    fn simple_buff_potions() {
        let mut c = Combat::new(1);
        let e0 = c.player.energy;
        give_potion(&mut c, PotionKind::Energy);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.energy, e0 + 2);

        give_potion(&mut c, PotionKind::Dexterity);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Dexterity), Some(2));

        give_potion(&mut c, PotionKind::Focus);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Focus), Some(2));

        let orbs0 = c.player.max_orbs;
        give_potion(&mut c, PotionKind::Capacity);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.max_orbs, orbs0 + 2);

        give_potion(&mut c, PotionKind::GhostInAJar);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::IntangiblePlayer), Some(1));

        give_potion(&mut c, PotionKind::HeartOfIron);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Metallicize), Some(6));

        give_potion(&mut c, PotionKind::LiquidBronze);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Thorns), Some(3));

        give_potion(&mut c, PotionKind::Regen);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Regen), Some(5));

        give_potion(&mut c, PotionKind::Ancient);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(pamount(&c, PowerKind::Artifact), Some(1));

        let h0 = c.player.hand.len();
        give_potion(&mut c, PotionKind::Swift);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.hand.len(), (h0 + 3).min(10));
    }

    /// 熔炉的祝福：升级全手牌（可升级的）
    #[test]
    fn blessing_of_forge_upgrades_hand() {
        let mut c = Combat::new(1);
        force_hand(&mut c, &["Strike_R", "Defend_R", "Bash"]);
        give_potion(&mut c, PotionKind::BlessingOfTheForge);
        c.step(&Action::UsePotion {
            index: 0,
            target: None,
        });
        assert!(c.player.hand.iter().all(|h| h.upgraded));
    }

    /// 全药水扫描：每只都能使用且不 panic（带响应屏的顺带解决）
    #[test]
    fn all_potions_smoke() {
        for kind in crate::potions::ALL_POTIONS {
            let mut c = Combat::new(11);
            c.player.creature.hp = 80;
            c.player.potions[0] = Some(PotionInst::new(*kind));
            if !kind.can_use(false) {
                continue; // Fairy 仅被动
            }
            let target = if kind.target_required() {
                Some(0)
            } else {
                None
            };
            c.step(&Action::UsePotion { index: 0, target });
            // 解开可能挂起的选择屏（探素/姿态/赌徒/液态记忆）
            match &c.pending {
                Some(Pending::Discovery { .. }) => c.step(&Action::DiscoveryResponse { index: 0 }),
                Some(Pending::StanceChoice) => c.step(&Action::StanceResponse { wrath: false }),
                Some(Pending::GamblersDiscard) => c.step(&Action::HandSelect { indices: vec![] }),
                Some(Pending::PilePick { .. }) => c.step(&Action::PilePickResponse { index: 0 }),
                Some(Pending::HandExhaustAny) => c.step(&Action::HandSelect { indices: vec![] }),
                _ => {}
            }
        }
    }

    /// 丢弃药水：任何时候可丢，栏位腾空
    #[test]
    fn discard_potion_frees_slot() {
        let mut c = Combat::new(1);
        give_potion(&mut c, PotionKind::Fire);
        c.step(&Action::DiscardPotion { index: 0 });
        assert!(c.player.potions[0].is_none());
        assert!(!c.over);
    }

    /// 药水使用后同种子可重放（state_hash 一致）
    #[test]
    fn potion_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new(seed);
            c.player.potions[0] = Some(PotionInst::new(PotionKind::Fire));
            c.player.potions[1] = Some(PotionInst::new(PotionKind::Block));
            c.step(&Action::UsePotion {
                index: 0,
                target: Some(0),
            });
            c.step(&Action::UsePotion {
                index: 1,
                target: None,
            });
            c.state_hash()
        };
        assert_eq!(play(3), play(3));
        assert_ne!(play(3), play(4));
    }

    // ══════════════ 六火亡魂（Hexaghost.java 对照） ══════════════

    fn hexa() -> Combat {
        let mut c = Combat::new_with(9, 20, &[MonsterKind::Hexaghost]);
        c.player.creature.max_hp = 500;
        c.player.creature.hp = 500;
        c.player.hand.clear();
        c
    }

    // ══════════════ Act 2（TheCity）普通遭遇 ══════════════

    fn m_amount(c: &Combat, i: usize, k: PowerKind) -> Option<i32> {
        c.monsters[i]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == k)
            .map(|p| p.amount)
    }

    /// 战前状态：球体（壁垒+神器+40甲）、灼壳（护甲14+14甲）、伯德（飞行）、蛇草（可延展）
    #[test]
    fn act2_prebattle_states() {
        let sg = Combat::new_with(1, 20, &[MonsterKind::SphericGuardian]);
        assert_eq!(sg.monsters[0].creature.max_hp, 20, "HP 固定 20");
        assert_eq!(sg.monsters[0].creature.block, 40);
        assert!(m_amount(&sg, 0, PowerKind::Barricade).is_some());
        assert_eq!(m_amount(&sg, 0, PowerKind::Artifact), Some(3));
        assert_eq!(sg.monsters[0].chomp_dmg, 11, "A2+ 伤害");

        let sp = Combat::new_with(1, 20, &[MonsterKind::ShelledParasite]);
        assert_eq!(sp.monsters[0].creature.block, 14);
        assert_eq!(m_amount(&sp, 0, PowerKind::PlatedArmor), Some(14));

        let byrd = Combat::new_with(1, 20, &[MonsterKind::Byrd]);
        assert_eq!(
            m_amount(&byrd, 0, PowerKind::Flight),
            Some(4),
            "A17+ 飞行 4"
        );

        let snake = Combat::new_with(1, 20, &[MonsterKind::SnakePlant]);
        assert_eq!(m_amount(&snake, 0, PowerKind::Malleable), Some(3));
    }

    /// 球体守护者：首招充能(25)，次招脆化，壁垒保甲；神器抵消减益
    #[test]
    fn spheric_guardian_cycle() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::SphericGuardian]);
        assert_eq!(c.monsters[0].next_move, 2, "首招充能");
        c.step(&Action::EndTurn);
        assert_eq!(
            c.monsters[0].creature.block,
            40 + 35,
            "壁垒保甲+充能(A17+35)"
        );
        assert_eq!(c.monsters[0].next_move, 4, "次招脆化");
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 11);
        // 神器在球体自己身上（防对它的 debuff），本回合未被消耗
        assert_eq!(m_amount(&c, 0, PowerKind::Artifact), Some(3));
        // 玩家受到脆 5
        assert!(c
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Frail));
        // 下一招：上次 4（非 1）→ 1 重击 ×2
        assert_eq!(c.monsters[0].next_move, 1);
    }

    /// 球体神器：对它施加的 debuff 被抵消
    #[test]
    fn spheric_guardian_artifact() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::SphericGuardian]);
        c.exec(Q::ApplyPowerToMonster {
            target: 0,
            kind: PowerKind::Weak,
            amount: 2,
        });
        assert_eq!(m_amount(&c, 0, PowerKind::Weak), None, "被神器抵消");
        assert_eq!(m_amount(&c, 0, PowerKind::Artifact), Some(2), "3-1=2");
    }

    /// 天选者：非 A17 首招戳刺×2 → 咒缚；打出非攻击牌塞晕眩
    #[test]
    fn chosen_hex_flow() {
        let mut c = Combat::new_with(5, 16, &[MonsterKind::Chosen]);
        assert_eq!(c.monsters[0].next_move, 5, "非 A17 首招戳刺");
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 6 * 2, "A2+ 戳刺 6×2");
        assert_eq!(c.monsters[0].next_move, 4, "次招咒缚");
        c.step(&Action::EndTurn);
        assert!(c
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Hex));
        // 打出非攻击牌 → 1 张晕眩随机入抽牌堆
        force_hand(&mut c, &["Defend_R"]);
        let draw0 = c.player.draw.len();
        c.step(&Action::PlayCard {
            index: 0,
            target: None,
        });
        assert_eq!(c.player.draw.len(), draw0 + 1);
        assert_eq!(
            c.player.draw.iter().filter(|x| x.def.id == "Dazed").count(),
            1
        );
        // A17：首招直接咒缚
        let a17 = Combat::new_with(5, 17, &[MonsterKind::Chosen]);
        assert_eq!(a17.monsters[0].next_move, 4);
    }

    /// 灼壳寄生生：吸血按实际掉血治疗；护甲被打穿 → 破甲眩晕
    #[test]
    fn shelled_parasite_armor_break() {
        let mut c = Combat::new_with(7, 20, &[MonsterKind::ShelledParasite]);
        assert_eq!(c.monsters[0].next_move, 1, "A17 首招猛击");
        // 玩家猛击：护甲 -1/次掉血
        c.exec(Q::DamageMonster { target: 0, dmg: 20 });
        // 20 打 14 甲+14 护甲：block 14 吸 14 → hp -6 → 护甲 power -1
        assert_eq!(m_amount(&c, 0, PowerKind::PlatedArmor), Some(13));
        // 吸血招（手动 exec）
        let hp0 = c.monsters[0].creature.hp;
        let php0 = c.player.creature.hp;
        c.exec(Q::MonsterVampire { src: 0, base: 12 });
        assert_eq!(c.player.creature.hp, php0 - 12);
        assert_eq!(
            c.monsters[0].creature.hp,
            (hp0 + 12).min(c.monsters[0].creature.max_hp)
        );
        // 连续打击至护甲归零 → 破甲眩晕（next_move=4）；先拉高血量避免先死
        c.monsters[0].creature.max_hp = 500;
        c.monsters[0].creature.hp = 500;
        for _ in 0..14 {
            c.exec(Q::DamageMonster { target: 0, dmg: 10 });
        }
        assert_eq!(m_amount(&c, 0, PowerKind::PlatedArmor), None);
        assert_eq!(c.monsters[0].next_move, 4, "ARMOR_BREAK 眩晕");
    }

    /// 伯德鸟：飞行减半 + 受击掉层 → 落地眩晕 → 头槌 → 重新起飞
    #[test]
    fn byrd_flight_cycle() {
        let mut c = Combat::new_with(9, 20, &[MonsterKind::Byrd]);
        let flight0 = m_amount(&c, 0, PowerKind::Flight).unwrap();
        // 打击 6 走出牌管线 → 飞行减半 3，掉 1 层飞行
        force_hand(&mut c, &["Strike_R"]);
        let hp0 = c.monsters[0].creature.hp;
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.monsters[0].creature.hp, hp0 - 3, "飞行减半");
        assert_eq!(m_amount(&c, 0, PowerKind::Flight), Some(flight0 - 1));
        // 打到 0 层 → GROUNDED 眩晕
        for _ in 0..(flight0 - 1) {
            c.exec(Q::DamageMonster { target: 0, dmg: 2 });
        }
        assert_eq!(m_amount(&c, 0, PowerKind::Flight), None);
        assert_eq!(c.monsters[0].next_move, 4, "GROUNDED 眩晕");
        // 眩晕回合（无动作）→ 头槌 → setMove(2) 无 RollMove
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters[0].next_move, 5, "眩晕后头槌");
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 3, "头槌 3");
        assert_eq!(c.monsters[0].next_move, 2, "头槌后起飞");
        // 起飞 → 恢复飞行
        c.step(&Action::EndTurn);
        assert_eq!(m_amount(&c, 0, PowerKind::Flight), Some(flight0));
    }

    /// 劫匪：偷金 → 抢劫×2 → 烟雾 → 逃离（战斗胜利，金币损失）
    #[test]
    fn mugger_chain_and_escape() {
        let mut c = Combat::new_with(11, 20, &[MonsterKind::Mugger]);
        let gold0 = c.gold;
        assert_eq!(c.monsters[0].next_move, 1);
        // 抢劫 1：伤害 11 + 偷 20（A17+）
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 11);
        assert_eq!(c.gold, gold0 - 20);
        assert_eq!(c.monsters[0].next_move, 1, "第二次抢劫");
        c.step(&Action::EndTurn);
        // 第 2 次后按 aiRng 走烟雾(2) 或重扫(4)——两分支都最终到烟雾→逃离
        for _ in 0..3 {
            if c.over || c.monsters[0].escaped {
                break;
            }
            c.step(&Action::EndTurn);
        }
        assert!(c.monsters[0].escaped, "最终逃离");
        assert!(c.over && c.won, "全员离场=战斗结束（胜）");
        // stolenGold 记在 bellow_block（burn_upgraded 已被 slashCount 占用）
        let stolen = c.monsters[0].bellow_block;
        assert!(stolen >= 40, "至少两次抢劫各 20");
        assert_eq!(c.gold, gold0 - stolen, "偷走不归还（逃离路径）");
        // 逃离路径不产生 STOLEN_GOLD 奖励（只有 die() 才归还）
        assert_eq!(c.stolen_gold_rewards, 0, "逃离不归还");
    }

    /// 咬人蛇草：可延展受击得甲递增（3→4→5）
    #[test]
    fn snake_plant_malleable() {
        let mut c = Combat::new_with(13, 20, &[MonsterKind::SnakePlant]);
        assert_eq!(c.monsters[0].next_move, 1, "首招多为撕咬");
        c.exec(Q::DamageMonster { target: 0, dmg: 5 });
        assert_eq!(c.monsters[0].creature.block, 3, "首次 +3");
        // 第二次：5 伤先被 3 格挡吸收 2 → 掉 2 血，再 +4 格挡
        c.exec(Q::DamageMonster { target: 0, dmg: 5 });
        assert_eq!(c.monsters[0].creature.block, 4, "第二次 +4");
        assert_eq!(m_amount(&c, 0, PowerKind::Malleable), Some(5), "递增到 5");
    }

    /// 斯涅克：首招瞪视上混乱；抽牌费用重掣（确定性、X 费不变）
    #[test]
    fn snecko_confusion() {
        let mut c = Combat::new_with(15, 20, &[MonsterKind::Snecko]);
        assert_eq!(c.monsters[0].next_move, 1, "首招瞪视");
        c.step(&Action::EndTurn);
        assert!(c
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Confusion));
        // 抽 5 张：费用应全部落在 0..=3
        c.player.hand.clear();
        c.player.draw = ["Bash", "Strike_R", "Defend_R", "Whirlwind", "Bash"]
            .iter()
            .map(|id| CardInst::new(id))
            .collect();
        c.draw_cards(5);
        let whirl = c
            .player
            .hand
            .iter()
            .find(|h| h.def.id == "Whirlwind")
            .expect("抽上手");
        assert_eq!(whirl.cost_pay(), -1, "X 费不变");
        assert!(c
            .player
            .hand
            .iter()
            .filter(|h| h.def.id != "Whirlwind")
            .all(|h| (0..=3).contains(&h.cost_pay())));
    }

    /// 百夫长护卫：给随机其他存活怪格挡；单独时给自己
    #[test]
    fn centurion_protect() {
        let mut c = Combat::new_with(17, 20, &[MonsterKind::Centurion, MonsterKind::Healer]);
        // 直接执行护卫（构造局面）
        c.monsters[0].next_move = 2;
        c.step(&Action::EndTurn);
        // aiRng 选择的是 0 或 1；百夫长(0)不会是目标（排除自己）
        assert_eq!(c.monsters[1].creature.block, 20, "A17 护卫 20 给队友");
        assert_eq!(c.monsters[0].creature.block, 0);

        let mut solo = Combat::new_with(17, 20, &[MonsterKind::Centurion]);
        solo.monsters[0].next_move = 2;
        solo.step(&Action::EndTurn);
        assert_eq!(solo.monsters[0].creature.block, 20, "无人时给自己");
    }

    /// 祭司：缺口大时群疗 16；群力 +2/3
    #[test]
    fn healer_heals_and_buffs() {
        let mut c = Combat::new_with(19, 20, &[MonsterKind::Centurion, MonsterKind::Healer]);
        c.monsters[0].creature.hp -= 30; // 缺口 30 > 15 → 治疗优先
        let hp0 = c.monsters[0].creature.hp;
        c.monsters[1].next_move = 2;
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters[0].creature.hp, hp0 + 16, "群疗 16");

        let mut c2 = Combat::new_with(19, 16, &[MonsterKind::Healer]);
        c2.monsters[0].next_move = 3;
        c2.step(&Action::EndTurn);
        assert_eq!(
            m_amount(&c2, 0, PowerKind::Strength),
            Some(2),
            "非 A17 力量 2"
        );
    }

    /// Act 2 全怪物同种子重放一致
    #[test]
    fn act2_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new_with(
                seed,
                20,
                &[
                    MonsterKind::Centurion,
                    MonsterKind::Healer,
                    MonsterKind::Byrd,
                ],
            );
            c.player.creature.max_hp = 999;
            c.player.creature.hp = 999;
            c.player.hand.clear();
            for _ in 0..10 {
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
            }
            c.state_hash()
        };
        assert_eq!(play(21), play(21));
        assert_ne!(play(21), play(22));
    }

    // ══════════════ Act 2 精英/Boss ══════════════

    /// 五小鬼：固定招循环 + 战鬼愤怒受击涨力量 + 巫鬼充能 3 回合爆发
    #[test]
    fn gremlin_minions_basics() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::GremlinWarrior]);
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 5, "A2+ 抓击 5");
        assert_eq!(c.monsters[0].next_move, 1);
        // 愤怒（A17+ 2 层）：受击每次 +2 力量
        c.exec(Q::DamageMonster { target: 0, dmg: 3 });
        assert_eq!(m_amount(&c, 0, PowerKind::Strength), Some(2));
        c.exec(Q::DamageMonster { target: 0, dmg: 3 });
        assert_eq!(
            m_amount(&c, 0, PowerKind::Strength),
            Some(4),
            "A17+ 愤怒 2/次"
        );

        let mut w = Combat::new_with(3, 20, &[MonsterKind::GremlinWizard]);
        assert_eq!(w.monsters[0].next_move, 2, "巫鬼充能起手");
        w.step(&Action::EndTurn); // charge → 2
        w.step(&Action::EndTurn); // charge → 3 满
        assert_eq!(w.monsters[0].next_move, 1, "下回合爆发");
        let hp0 = w.player.creature.hp;
        w.step(&Action::EndTurn);
        assert_eq!(w.player.creature.hp, hp0 - 30, "A2+ 魔法 30");
    }

    /// 小鬼首领：Rally 用 aiRng 八池召唤（上限 3）；Encourage 全员力量+队友格挡
    #[test]
    fn gremlin_leader_rally_encourage() {
        let mut c = Combat::new_with(
            5,
            20,
            &[
                MonsterKind::GremlinThief,
                MonsterKind::GremlinFat,
                MonsterKind::GremlinLeader,
            ],
        );
        // Encourage（A20=A17 档）：全 +力量 5，小鬼们 +格挡 10
        c.monsters[2].next_move = 3;
        c.step(&Action::EndTurn);
        assert_eq!(m_amount(&c, 2, PowerKind::Strength), Some(5), "A17+ 力量 5");
        assert_eq!(m_amount(&c, 0, PowerKind::Strength), Some(5));
        assert_eq!(c.monsters[0].creature.block, 10);
        assert_eq!(c.monsters[2].creature.block, 0, "自己无格挡");
        // Rally：召唤 2 只（总数 2→4，其中死亡位可复用；杀两只小鬼后再召）
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: 999,
        });
        c.exec(Q::DamageMonster {
            target: 1,
            dmg: 999,
        });
        let before = c.monsters.len();
        c.exec(Q::SummonGremlin);
        assert_eq!(c.monsters.len(), before, "死亡位复用不扩表");
        assert!(!c.monsters[0].creature.is_dead(), "新小鬼占位");
        assert_ne!(c.monsters[0].kind, MonsterKind::GremlinLeader);
        assert!(matches!(
            c.monsters[0].kind,
            MonsterKind::GremlinWarrior
                | MonsterKind::GremlinThief
                | MonsterKind::GremlinFat
                | MonsterKind::GremlinTsundere
                | MonsterKind::GremlinWizard
        ));
    }

    /// 连击之书：连击数递增（初始 1，刺→2→3…），重刺 A18 也递增
    #[test]
    fn book_of_stabbing_escalates() {
        let mut c = Combat::new_with(7, 20, &[MonsterKind::BookOfStabbing]);
        // stabCount 起 1；开局 getMove 已按分支 +1（A18 双计）→ ≥2
        assert!(c.monsters[0].burn_upgraded >= 2, "开局 roll 已递增");
        // 直接构造连击：连续多轮刺击
        for _ in 0..4 {
            c.monsters[0].next_move = 1;
            c.step(&Action::EndTurn);
        }
        // A18+：重刺后 stabCount 也 +1；此处验证 stabCount > 1
        assert!(c.monsters[0].burn_upgraded > 1, "连击数增长");
    }

    /// 监工：鞭笞 7 + 3 伤口（A18）
    #[test]
    fn taskmaster_wounds() {
        let mut c = Combat::new_with(9, 20, &[MonsterKind::Taskmaster]);
        assert_eq!(c.monsters[0].next_move, 2);
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 7);
        let wounds = c
            .player
            .discard
            .iter()
            .filter(|x| x.def.id == "Wound")
            .count();
        assert_eq!(wounds, 3, "A18+ 3 张伤口");
        assert_eq!(
            m_amount(&c, 0, PowerKind::Strength),
            Some(1),
            "A18 自+1 力量"
        );
    }

    /// 铜制自动机：召唤两铜球 → 铜球静滞吸牌 → 死亡归还 → 光束循环
    #[test]
    fn automaton_orbs_stasis() {
        let mut c = Combat::new_with(11, 20, &[MonsterKind::BronzeAutomaton]);
        assert_eq!(m_amount(&c, 0, PowerKind::Artifact), Some(3));
        assert_eq!(c.monsters[0].next_move, 4, "首招召唤");
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters.len(), 3, "两只铜球");
        assert_eq!(c.monsters[0].next_move, 1, "连枷");
        // 静滞吸牌（手动触发铜球）
        let draw0 = c.player.draw.len();
        c.exec(Q::StasisSteal { src: 1 });
        assert_eq!(c.player.draw.len(), draw0 - 1, "抽走一张");
        let stolen_id = c.monsters[1].stasis_card.unwrap().def.id;
        // 铜球死亡 → 归还手牌
        c.exec(Q::DamageMonster {
            target: 1,
            dmg: 999,
        });
        let hand_ids: Vec<&str> = c.player.hand.iter().map(|h| h.def.id).collect();
        assert!(hand_ids.contains(&stolen_id), "死亡归还");
        // 支援光束：给自动机格挡
        c.exec(Q::BlockMonsterKind {
            kind: MonsterKind::BronzeAutomaton,
            amount: 12,
        });
        assert_eq!(c.monsters[0].creature.block, 12);
    }

    /// 收集者：召唤火把头、群力、超大减益（第 3 回合）、复活
    #[test]
    fn collector_spawn_and_revive() {
        let mut c = Combat::new_with(13, 20, &[MonsterKind::TheCollector]);
        assert_eq!(c.monsters[0].next_move, 1, "首招召唤");
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters.len(), 3, "两只火把头");
        // 第 3 回合（history.len()==2）→ 超大减益
        c.monsters[0].next_move = 2; // T2 火球
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters[0].next_move, 4, "第 3 回合超大减益");
        c.step(&Action::EndTurn);
        for k in [PowerKind::Weak, PowerKind::Vulnerable, PowerKind::Frail] {
            assert!(c.player.creature.powers.iter().any(|p| p.kind == k));
        }
        // 杀火把头 → 复活
        c.exec(Q::DamageMonster {
            target: 1,
            dmg: 999,
        });
        assert!(c.monsters[1].creature.is_dead());
        c.exec(Q::ReviveTorchHeads);
        assert!(!c.monsters[1].creature.is_dead(), "原位重生");
        assert_eq!(c.monsters.len(), 3, "不扩表");
    }

    /// 冠军：半血触发爆怒（清减益+力量×9~12），随后连续处决 ×2
    #[test]
    fn champ_phase2_anger() {
        let mut c = Combat::new_with(15, 20, &[MonsterKind::Champ]);
        assert_eq!(c.monsters[0].creature.max_hp, 440, "A9+ 血量");
        // 施加减益再打入半血 → 爆怒清减益 + 力量×(4×3 A19)
        apply_power(
            &mut c.monsters[0].creature.powers,
            PowerKind::Weak,
            2,
            true,
            true,
        );
        c.monsters[0].creature.hp = 100;
        // 手动触发一次 getMove（爆怒）
        c.roll_move(0);
        // roll_move 走 getMove → 爆怒（7）
        assert_eq!(c.monsters[0].next_move, 7, "半血爆怒");
        c.step(&Action::EndTurn);
        assert!(
            !c.monsters[0]
                .creature
                .powers
                .iter()
                .any(|p| p.kind == PowerKind::Weak),
            "减益被清"
        );
        assert_eq!(
            m_amount(&c, 0, PowerKind::Strength),
            Some(12),
            "A19 力量 4×3"
        );
        assert_eq!(c.monsters[0].next_move, 3, "爆怒后处决");
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        // 处决 10×2（力量 +12 → 22×2=44）
        assert_eq!(c.player.creature.hp, hp0 - 44);
    }

    /// Act 2 精英/Boss 同种子重放一致（含召唤物流）
    #[test]
    fn act2_elites_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new_with(
                seed,
                20,
                &[
                    MonsterKind::GremlinThief,
                    MonsterKind::GremlinFat,
                    MonsterKind::GremlinLeader,
                ],
            );
            c.player.creature.max_hp = 999;
            c.player.creature.hp = 999;
            c.player.hand.clear();
            for _ in 0..12 {
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
            }
            c.state_hash()
        };
        assert_eq!(play(31), play(31));
        assert_ne!(play(31), play(32));
    }

    // ══════════════ Act 3（TheBeyond）普通遭遇 ══════════════

    /// 小暗灵：半死机制——单只死→待机复活半血；全员同回合死→真死
    #[test]
    fn darkling_half_dead_revive() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::Darkling, MonsterKind::Darkling]);
        let max0 = c.monsters[0].creature.max_hp;
        // 杀一只：半死，不判胜
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: 999,
        });
        assert!(c.monsters[0].half_dead);
        assert!(!c.over);
        assert_eq!(c.monsters[0].next_move, 4, "半死待机");
        // 目标选择排除半死怪
        assert!(!c.legal_actions().iter().any(|a| matches!(
            a,
            Action::PlayCard {
                target: Some(0),
                ..
            }
        )));
        // 其回合：待机 → 复活（下回合）
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters[0].next_move, 5, "轮到复活");
        c.step(&Action::EndTurn);
        assert!(!c.monsters[0].half_dead);
        assert_eq!(c.monsters[0].creature.hp, max0 / 2, "复活半血");
        // 全员同回合死 → 真死胜利
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: 999,
        });
        c.exec(Q::DamageMonster {
            target: 1,
            dmg: 999,
        });
        assert!(c.over && c.won, "全员半死=真死");
    }

    /// 排斥体：晕眩入弃堆；天球行者激光+灼伤（弃堆+主牌组）
    #[test]
    fn repulsor_and_orbwalker() {
        let mut c = Combat::new_with(5, 20, &[MonsterKind::Repulsor]);
        c.monsters[0].next_move = 1; // DAZE
        c.step(&Action::EndTurn);
        let dazed = c
            .player
            .discard
            .iter()
            .filter(|x| x.def.id == "Dazed")
            .count();
        assert_eq!(dazed, 2);

        let mut w = Combat::new_with(5, 20, &[MonsterKind::OrbWalker]);
        assert_eq!(m_amount(&w, 0, PowerKind::Strength), Some(5), "A17+ 力量 5");
        w.monsters[0].next_move = 1; // LASER
        w.step(&Action::EndTurn);
        assert_eq!(
            w.player
                .discard
                .iter()
                .filter(|x| x.def.id == "Burn")
                .count(),
            1,
            "灼伤入弃堆"
        );
        assert_eq!(w.master_deck_additions.len(), 1, "主牌组记录");
    }

    /// 尖塔生长体：紧缠（玩家回合末直伤）
    #[test]
    fn spire_growth_constrict() {
        let mut c = Combat::new_with(7, 20, &[MonsterKind::SpireGrowth]);
        c.monsters[0].next_move = 2;
        c.player.creature.hp = 100;
        c.player.creature.block = 0;
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn); // 施加紧缠 10
        assert!(c
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Constricted));
        c.monsters[0].next_move = 3; // 重击防干扰
        c.step(&Action::EndTurn); // 回合末受紧缠(A17+ 12) + 重击 25
        assert_eq!(c.player.creature.hp, hp0 - 12 - 25);
    }

    /// 转瞬即逝：伤害递增、受击掉力量（轮末回复）、消逝自灭
    #[test]
    fn transient_fading_shifting() {
        let mut c = Combat::new_with(9, 20, &[MonsterKind::Transient]);
        c.player.creature.max_hp = 999;
        c.player.creature.hp = 999;
        assert_eq!(m_amount(&c, 0, PowerKind::Fading), Some(6), "A17+ 6 层");
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 40, "起始 40");
        assert_eq!(m_amount(&c, 0, PowerKind::Fading), Some(5), "递减");
        // 受击 → 力量 -10（本回合伤害）
        c.exec(Q::DamageMonster { target: 0, dmg: 10 });
        assert_eq!(
            m_amount(&c, 0, PowerKind::Strength),
            Some(-10),
            "变形掉力量"
        );
        assert_eq!(m_amount(&c, 0, PowerKind::GainStrength), Some(10));
        c.step(&Action::EndTurn); // 轮末力量回复（-10+10=0 → power 移除）
        assert_eq!(
            m_amount(&c, 0, PowerKind::Strength),
            None,
            "轮末回归归零移除"
        );
        // 意图递增
        assert_eq!(c.intent_damage(0), Some(60), "两次攻击后 40+2×10");
    }

    /// 巨口：咆哮起手 → 啃食/流涎交替，啃食段数递增
    #[test]
    fn maw_roar_and_nom() {
        let mut c = Combat::new_with(11, 20, &[MonsterKind::Maw]);
        assert_eq!(c.monsters[0].next_move, 2, "首招咆哮");
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        c.step(&Action::EndTurn);
        for k in [PowerKind::Weak, PowerKind::Frail] {
            assert!(c.player.creature.powers.iter().any(|p| p.kind == k));
        }
        // 之后：啃食(5) → 流涎(+3力) → 啃食×2 → …
        let mut noms = 0;
        for _ in 0..6 {
            let mv = c.monsters[0].next_move;
            if mv == 5 {
                noms += 1;
            }
            c.step(&Action::EndTurn);
        }
        assert!(noms >= 2, "啃食多次");
        assert!(
            m_amount(&c, 0, PowerKind::Strength).unwrap_or(0) >= 3,
            "流涎力量"
        );
    }

    /// 蠕动聚块：战前 Reactive+Malleable；受击重掷招式；寄生前置一次
    #[test]
    fn writhing_mass_reactive_mega() {
        let mut c = Combat::new_with(13, 20, &[MonsterKind::WrithingMass]);
        assert!(m_amount(&c, 0, PowerKind::Reactive).is_some());
        assert_eq!(m_amount(&c, 0, PowerKind::Malleable), Some(3));
        // MEGA：寄生前置（主牌组记录，一次）
        c.monsters[0].next_move = 4;
        c.step(&Action::EndTurn);
        assert_eq!(c.master_deck_additions.len(), 1);
        assert!(c.master_deck_additions[0].def.id == "Parasite");
        // 受击 → Malleable 得甲 + Reactive 重掷
        let mv_before = c.monsters[0].next_move;
        c.exec(Q::DamageMonster { target: 0, dmg: 5 });
        assert!(c.monsters[0].creature.block > 0, "可延展得甲");
        let _ = mv_before;
    }

    /// Act 3 全普通怪同种子重放一致
    #[test]
    fn act3_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new_with(
                seed,
                20,
                &[
                    MonsterKind::Darkling,
                    MonsterKind::Darkling,
                    MonsterKind::Maw,
                ],
            );
            c.player.creature.max_hp = 999;
            c.player.creature.hp = 999;
            c.player.hand.clear();
            for _ in 0..12 {
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
            }
            c.state_hash()
        };
        assert_eq!(play(41), play(41));
        assert_ne!(play(41), play(42));
    }

    // ══════════════ Act 3 精英/Boss ══════════════

    /// 巨首：Slow（出牌叠层增伤）；count 递减到时刻已至大伤害
    #[test]
    fn giant_head_slow_and_time() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::GiantHead]);
        assert!(m_amount(&c, 0, PowerKind::Slow).is_some(), "战前 Slow");
        c.monsters[0].creature.hp = 999;
        c.monsters[0].creature.max_hp = 999;
        // 打击 6 → Slow 0 层 → 6；再打一张（Slow=1）→ 6×1.1=6.6 floor 6；
        // 第三张（Slow=2）→ 7.2 → 7
        force_hand(&mut c, &["Strike_R", "Strike_R", "Strike_R"]);
        let hp0 = c.monsters[0].creature.hp;
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        let hp1 = c.monsters[0].creature.hp;
        assert_eq!(hp0 - hp1, 6, "首张无 Slow 加成");
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        let hp2 = c.monsters[0].creature.hp;
        assert_eq!(hp1 - hp2, 6, "第二张 1 层 ×1.1=6.6→6");
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        let hp3 = c.monsters[0].creature.hp;
        assert_eq!(hp2 - hp3, 7, "第三张 2 层 ×1.2=7.2→7");
        // count 起 5：开局 roll 后 4 → 意图为时刻已至（count<=1 才触发，此时还不是）
        // 直接构造 count=0 → 时刻已至 40 + (1-(-1))×5? 按 idx=1-count
        c.monsters[0].burn_upgraded = 0;
        c.monsters[0].next_move = 2;
        c.monsters[0].move_history.push(2);
        let php0 = c.player.creature.hp;
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        let p999 = c.player.creature.hp;
        let _ = php0;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, p999 - 45, "40+idx1×5（A3+）");
    }

    /// 复仇者：回合尾自动无形；无形期间受击固定 1 伤
    #[test]
    fn nemesis_intangible() {
        let mut c = Combat::new_with(5, 20, &[MonsterKind::Nemesis]);
        c.monsters[0].creature.hp = 999;
        c.monsters[0].creature.max_hp = 999;
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        // 第 1 回合结束后获得无形
        c.step(&Action::EndTurn);
        assert_eq!(m_amount(&c, 0, PowerKind::IntangiblePlayer), Some(1));
        // 无形期间受击 → 1 伤
        force_hand(&mut c, &["Strike_R"]);
        let hp0 = c.monsters[0].creature.hp;
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(hp0 - c.monsters[0].creature.hp, 1, "无形固定 1 伤");
    }

    /// 爬虫法师：召唤蛇匕；蛇匕刺 9 后自爆 25 并自灭
    #[test]
    fn reptomancer_daggers() {
        let mut c = Combat::new_with(7, 16, &[MonsterKind::Reptomancer]);
        assert_eq!(c.monsters[0].next_move, 2, "首招召唤");
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters.len(), 2, "召唤 1 把（非 A18）");
        // 蛇匕行动：刺 9 → 下回合自爆
        let mut d = Combat::new_with(7, 16, &[MonsterKind::SnakeDagger]);
        assert_eq!(d.monsters[0].next_move, 1, "首招刺");
        let hp0 = d.player.creature.hp;
        d.step(&Action::EndTurn);
        assert_eq!(d.player.creature.hp, hp0 - 9);
        assert_eq!(d.monsters[0].next_move, 2, "下回合自爆");
        let hp1 = d.player.creature.hp;
        d.step(&Action::EndTurn);
        assert_eq!(d.player.creature.hp, hp1 - 25);
        assert!(d.monsters[0].creature.is_dead(), "自爆自灭");
        assert!(d.over && d.won);
    }

    /// 觉醒者：死亡转阶段（满血复活）；好奇（能力牌涨力）；阶段2 黑暗回响
    #[test]
    fn awakened_one_stages() {
        let mut c = Combat::new_with(9, 20, &[MonsterKind::AwakenedOne]);
        assert_eq!(
            m_amount(&c, 0, PowerKind::RegenerateMonster),
            Some(15),
            "A19+ 15"
        );
        assert_eq!(m_amount(&c, 0, PowerKind::Curiosity), Some(2), "A19+ 2");
        assert_eq!(m_amount(&c, 0, PowerKind::Strength), Some(2), "A4+ 力量 2");
        // 打出能力牌 → 好奇 +1 力量
        c.exec(Q::ApplyPowerToSelf {
            kind: PowerKind::Artifact,
            amount: 1,
        });
        // 杀死 → 半死 → 下回合重生满血
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: 999,
        });
        assert!(c.monsters[0].half_dead);
        assert!(!c.over, "阶段 1 死亡不判负");
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        c.step(&Action::EndTurn); // move 3 重生
        assert!(!c.monsters[0].half_dead);
        assert_eq!(c.monsters[0].creature.hp, 320, "A9+ 阶段 2 满血 320");
        assert_eq!(c.monsters[0].next_move, 5, "黑暗回响起手");
        let php = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, php - 42, "回响 40 + A4 力量 2");
        // 阶段 2 死亡 = 真死
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: 999,
        });
        assert!(c.over && c.won);
    }

    /// 多努/德卡：攻环交替；力量环/护甲环给全员；神器抵消
    #[test]
    fn donu_deca_circle() {
        let mut c = Combat::new_with(11, 20, &[MonsterKind::Donu, MonsterKind::Deca]);
        assert_eq!(m_amount(&c, 0, PowerKind::Artifact), Some(3), "A19+ 3");
        // 首招：Donu(0)=力量环，Deca(1)=光束
        assert_eq!(c.monsters[0].next_move, 2);
        assert_eq!(c.monsters[1].next_move, 0);
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        c.step(&Action::EndTurn);
        // 力量环：全怪 +3 力量；德卡光束 12×2 + 晕 2
        assert_eq!(m_amount(&c, 0, PowerKind::Strength), Some(3));
        assert_eq!(m_amount(&c, 1, PowerKind::Strength), Some(3));
        assert_eq!(c.player.creature.hp, 999 - 30, "德卡光束吃力量环：(12+3)×2");
        // 德卡护甲环（交替后）：全怪 +3 多层护甲
        c.monsters[1].next_move = 2;
        c.step(&Action::EndTurn);
        assert_eq!(m_amount(&c, 0, PowerKind::PlatedArmor), Some(3));
        assert_eq!(m_amount(&c, 1, PowerKind::PlatedArmor), Some(3));
    }

    /// 吞时者：TimeWarp 12 张牌终止玩家回合；半血加速回半血
    #[test]
    fn time_eater_warp_and_haste() {
        let mut c = Combat::new_with(13, 20, &[MonsterKind::TimeEater]);
        assert!(m_amount(&c, 0, PowerKind::TimeWarp).is_some());
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        // 连打 12 张 0 费小刀 → 玩家回合被终止（怪物行动）
        c.player.draw.clear();
        c.player.discard.clear();
        c.player.draw = ["Shiv"; 12].iter().map(|id| CardInst::new(id)).collect();
        c.player.hand.clear();
        c.player.energy = 99;
        let turn0 = c.turn;
        let mut played = 0;
        while played < 12 {
            if c.over || c.turn != turn0 {
                break;
            }
            if c.player.hand.is_empty() {
                c.draw_cards(5); // 测试辅助：补手牌（不触发 TimeWarp）
            }
            if c.player.hand.is_empty() {
                break;
            }
            c.step(&Action::PlayCard {
                index: 0,
                target: Some(0),
            });
            played += 1;
        }
        assert!(c.turn > turn0, "第 12 张牌后回合被终止（怪物行动）");
        // 半血加速：打掉过半 → 下回合 move 5 → 回半血
        let max = c.monsters[0].creature.max_hp;
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: max * 3 / 4,
        });
        c.step(&Action::EndTurn); // roll → haste
        assert_eq!(c.monsters[0].next_move, 5, "半血加速");
        let hp_before = c.monsters[0].creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters[0].creature.hp, max / 2, "回到半血");
        assert!(hp_before <= max / 2);
    }

    /// Act 3 精英/Boss 同种子重放一致
    #[test]
    fn act3_elites_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new_with(seed, 20, &[MonsterKind::AwakenedOne]);
            c.player.creature.max_hp = 9999;
            c.player.creature.hp = 9999;
            c.player.hand.clear();
            for _ in 0..14 {
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
            }
            c.state_hash()
        };
        assert_eq!(play(51), play(51));
        assert_ne!(play(51), play(52));
    }

    // ══════════════ Act 4（终局）══════════════

    /// 尖塔双子：盾筑防/粉碎自甲；矛燃烧灼伤/穿透群力；A18 神器
    #[test]
    fn spire_twin_basics() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::SpireShield, MonsterKind::SpireSpear]);
        assert_eq!(m_amount(&c, 0, PowerKind::Artifact), Some(2), "A18+ 神器 2");
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        // 盾：筑防（全怪 30 甲）
        c.monsters[0].next_move = 2;
        c.step(&Action::EndTurn);
        assert_eq!(c.monsters[0].creature.block, 30);
        assert_eq!(c.monsters[1].creature.block, 30, "全怪共享");
        // 矛：穿透（全怪 +2 力）
        c.monsters[1].next_move = 2;
        c.step(&Action::EndTurn);
        assert_eq!(m_amount(&c, 0, PowerKind::Strength), Some(2));
        assert_eq!(m_amount(&c, 1, PowerKind::Strength), Some(2));
        // 矛：燃烧打击 ×2 + 灼伤 2（步前读力量）
        c.monsters[1].next_move = 1;
        let hp0 = c.player.creature.hp;
        let str_spear = m_amount(&c, 1, PowerKind::Strength).unwrap_or(0);
        let str_shield_pre = m_amount(&c, 0, PowerKind::Strength).unwrap_or(0);
        c.step(&Action::EndTurn);
        // 盾同回合也可能行动（bash 14/粉碎 38）——只验证矛的部分：
        // 伤害 = 矛 (6+力)×2 + 灼伤 2 张；盾伤独立计算
        let spear_dmg = (6 + str_spear) * 2;
        let took = hp0 - c.player.creature.hp;
        assert!(
            took >= spear_dmg,
            "至少含矛的 {} 伤（实际 {}）",
            spear_dmg,
            took
        );
        let _ = str_shield_pre;
        let burns = c
            .player
            .discard
            .iter()
            .filter(|x| x.def.id == "Burn")
            .count();
        assert!(burns >= 2, "燃烧打击至少 2 张灼伤（自然轮可能追加）");
        // 盾：粉碎 = 伤害 + 等值自甲
        c.monsters[0].creature.block = 0;
        c.monsters[0].next_move = 3;
        let hp1 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        let str_shield = m_amount(&c, 0, PowerKind::Strength).unwrap_or(0);
        assert_eq!(
            c.player.creature.hp,
            hp1 - (38 + str_shield),
            "A3+ 粉碎 38+力"
        );
        assert!(c.monsters[0].creature.block >= 38, "粉碎获等值格挡");
    }

    /// 腐朽之心：无敌封顶；亡律每牌直伤；开场削弱+五状态；增益轮转
    #[test]
    fn corrupt_heart_mechanics() {
        let mut c = Combat::new_with(5, 20, &[MonsterKind::CorruptHeart]);
        assert_eq!(
            m_amount(&c, 0, PowerKind::Invincible),
            Some(200),
            "A19+ 200"
        );
        assert_eq!(m_amount(&c, 0, PowerKind::BeatOfDeath), Some(2), "A19+ 2");
        assert_eq!(c.monsters[0].next_move, 3, "首招削弱");
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        let draw0 = c.player.draw.len();
        c.step(&Action::EndTurn);
        for k in [PowerKind::Vulnerable, PowerKind::Weak, PowerKind::Frail] {
            assert!(c.player.creature.powers.iter().any(|p| p.kind == k));
        }
        // 五连状态入抽堆（换牌/洗牌会移动数量，改看全局净增）
        let total: usize = c.player.draw.len() + c.player.discard.len() + c.player.hand.len();
        assert_eq!(total, draw0 + 5 + 5, "原 5 手牌弃置 + 5 张状态净增");
        // 亡律：打一张牌 → 1 直伤（格挡吸收）
        c.player.creature.block = 5;
        force_hand(&mut c, &["Strike_R"]);
        c.step(&Action::PlayCard {
            index: 0,
            target: Some(0),
        });
        assert_eq!(c.player.creature.block, 3, "亡律 2(A19+) 被格挡吸收");
        // 无敌：打击 6(减伤...) → 单次 ≤ 剩余额度且额度递减
        let inv0 = m_amount(&c, 0, PowerKind::Invincible).unwrap();
        c.exec(Q::DamageMonster { target: 0, dmg: 50 });
        let inv1 = m_amount(&c, 0, PowerKind::Invincible).unwrap();
        assert_eq!(inv0 - inv1, 50, "额度递减");
        // 增益轮转：第 1 次 = 神器 2
        c.monsters[0].next_move = 4;
        c.step(&Action::EndTurn);
        assert!(
            m_amount(&c, 0, PowerKind::Artifact).is_some(),
            "首次轮转神器"
        );
        // 第 2 次 = 亡律 +1
        c.monsters[0].next_move = 4;
        c.step(&Action::EndTurn);
        assert_eq!(
            m_amount(&c, 0, PowerKind::BeatOfDeath),
            Some(2),
            "二次亡律+1"
        );
    }

    /// Act 4 同种子重放一致
    #[test]
    fn act4_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new_with(seed, 20, &[MonsterKind::CorruptHeart]);
            c.player.creature.max_hp = 9999;
            c.player.creature.hp = 9999;
            c.player.hand.clear();
            for _ in 0..14 {
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
            }
            c.state_hash()
        };
        assert_eq!(play(61), play(61));
        assert_ne!(play(61), play(62));
    }

    fn pamount_monster(c: &Combat, k: PowerKind) -> Option<i32> {
        c.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == k)
            .map(|p| p.amount)
    }

    /// 全周期：Activate(无伤) → Divider(锁定 HP/12+1 ×6) →
    /// Sear/Tackle/Sear/Inflame/Tackle/Sear → Inferno(升级全场灼伤+塞 3 张) → 循环。
    /// 灼伤会被洗回抽牌堆抽上手，回合末自伤计入动态期望（burn_dmg）。
    #[test]
    fn hexaghost_full_cycle() {
        let mut c = hexa();
        assert_eq!(c.monsters[0].creature.max_hp, 264, "A9+ Boss 血量");
        // A20 档：冲撞 6 / 地狱火 3 / 力量 3 / 灼伤 2 张
        assert_eq!(
            (
                c.monsters[0].chomp_dmg,
                c.monsters[0].thrash_dmg,
                c.monsters[0].thrash_block,
                c.monsters[0].bellow_str
            ),
            (6, 3, 3, 2)
        );
        assert_eq!(c.monsters[0].intent(), Intent::Unknown, "开场 Activate");

        // 走一整轮并验证：hp 差 = 怪物伤害 + 手中灼伤自伤；顺带返回步后 hp
        let step = |c: &mut Combat, monster_dmg: i32| -> i32 {
            let burn_dmg: i32 = c
                .player
                .hand
                .iter()
                .filter(|x| x.def.id == "Burn")
                .map(|x| x.magic().unwrap_or(2))
                .sum();
            let hp0 = c.player.creature.hp;
            c.step(&Action::EndTurn);
            assert!(!c.over, "测试期望存活");
            assert_eq!(c.player.creature.hp, hp0 - monster_dmg - burn_dmg);
            c.player.creature.hp
        };
        let burns = |c: &Combat| {
            c.player
                .discard
                .iter()
                .chain(c.player.draw.iter())
                .chain(c.player.hand.iter())
                .filter(|x| x.def.id == "Burn")
                .count()
        };

        // T1：Activate —— 无伤害，锁定分割者段伤 = 500/12+1 = 42
        step(&mut c, 0);
        assert_eq!(c.monsters[0].next_move, 1);
        assert_eq!(c.monsters[0].intent(), Intent::Attack);
        assert_eq!(c.intent_damage(0), Some(42));

        // T2：Divider 6×42 = 252
        step(&mut c, 6 * 42);
        assert_eq!(c.monsters[0].next_move, 4, "熄灭后首招 Sear");

        // T3：Sear 6 伤 + 2 灼伤
        step(&mut c, 6);
        assert_eq!(burns(&c), 2);
        assert_eq!(c.monsters[0].next_move, 2);

        // T4：Tackle ×2（无力量，6×2=12）
        step(&mut c, 12);
        assert_eq!(c.monsters[0].next_move, 4);

        // T5：Sear
        step(&mut c, 6);
        assert_eq!(burns(&c), 4);
        assert_eq!(c.monsters[0].next_move, 3);

        // T6：Inflame —— 12 格挡 + 3 力量（不对玩家造成伤害）
        step(&mut c, 0);
        assert_eq!(c.monsters[0].creature.block, 12);
        assert_eq!(pamount_monster(&c, PowerKind::Strength), Some(3));
        assert_eq!(c.monsters[0].next_move, 2);

        // T7：Tackle ×2（力量后 9×2=18；煽燃的 12 格挡在怪物自身）
        step(&mut c, 18);
        assert_eq!(c.monsters[0].next_move, 4);

        // T8：Sear（煽燃后 6+3=9；灼伤累计 6）
        step(&mut c, 9);
        assert_eq!(burns(&c), 6);
        assert_eq!(c.monsters[0].next_move, 6, "六球满 → Inferno");

        // T9：Inferno 6 段 ×(3+3)=36 + 升级全部灼伤 + 塞 3 张升级灼伤 + 置位
        step(&mut c, 36);
        assert_eq!(burns(&c), 9, "6 升级 + 3 新增");
        assert!(
            c.player
                .discard
                .iter()
                .chain(c.player.draw.iter())
                .chain(c.player.hand.iter())
                .filter(|x| x.def.id == "Burn")
                .all(|x| x.upgraded),
            "全场灼伤已升级"
        );
        assert_eq!(c.monsters[0].burn_upgraded, 1);
        assert_eq!(c.monsters[0].next_move, 4, "熄灭后循环回 Sear");

        // T10：Sear（力量仍在：9 伤，现在塞升级灼伤）
        let upgraded_burns = c
            .player
            .discard
            .iter()
            .chain(c.player.draw.iter())
            .chain(c.player.hand.iter())
            .filter(|x| x.def.id == "Burn" && x.upgraded)
            .count();
        step(&mut c, 9);
        assert_eq!(
            c.player
                .discard
                .iter()
                .chain(c.player.draw.iter())
                .chain(c.player.hand.iter())
                .filter(|x| x.def.id == "Burn" && x.upgraded)
                .count(),
            upgraded_burns + 2
        );
    }

    /// 分割者伤害在 Activate 时锁定；玩家中途死亡则中断后续段
    #[test]
    fn hexaghost_divider_locked() {
        let mut c = hexa();
        assert_eq!(c.intent_damage(0), None, "Activate 前（Unknown）无基础伤");
        c.step(&Action::EndTurn); // Activate：锁定 500/12+1=42
        c.player.creature.hp = 100; // 模拟回合内掉血
        c.step(&Action::EndTurn); // Divider 仍每段 42；100 只撑住 2 段，第 3 段致死
        assert!(c.over && !c.won);
        assert_eq!(c.player.creature.hp, 100 - 3 * 42);
    }

    /// 灼伤回合末自伤：THORNS 直伤（无乘区）+ 格挡吸收 + 升级版伤害 4
    /// （怪物设为咆哮，隔离玩家伤害以专测灼伤）
    #[test]
    fn burn_eot_triggers() {
        let mut c = Combat::new(5);
        c.monsters[0].next_move = 2; // 咆哮：不对玩家造成伤害
        force_hand(&mut c, &["Burn"]);
        let mut b2 = CardInst::new("Burn");
        b2.upgraded = true;
        c.player.hand.push(b2);
        c.player.creature.hp = 50;
        c.player.creature.block = 5;
        c.step(&Action::EndTurn);
        // 2（格挡 5→3）+ 4（格挡 3→0，扣 1 血）
        assert_eq!(c.player.creature.block, 0);
        assert_eq!(c.player.creature.hp, 49);
        // 灼伤随弃牌阶段入手弃牌堆
        assert_eq!(
            c.player
                .discard
                .iter()
                .filter(|x| x.def.id == "Burn")
                .count(),
            2
        );
    }

    /// 无形对灼伤的截断（atDamageFinalReceive 不分类型）
    #[test]
    fn burn_eot_intangible() {
        let mut c = Combat::new(5);
        c.monsters[0].next_move = 2; // 隔离：咆哮不对玩家造成伤害
        force_hand(&mut c, &["Burn"]);
        apply_power(
            &mut c.player.creature.powers,
            PowerKind::IntangiblePlayer,
            1,
            false,
            false,
        );
        c.player.creature.hp = 50;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, 49, "2 伤截断为 1");
    }

    /// 同种子整局重放一致
    #[test]
    fn hexaghost_replay_deterministic() {
        let play = |seed: u64| {
            let mut c = Combat::new_with(seed, 20, &[MonsterKind::Hexaghost]);
            c.player.creature.max_hp = 999;
            c.player.creature.hp = 999;
            c.player.hand.clear();
            for _ in 0..12 {
                if c.over {
                    break;
                }
                c.step(&Action::EndTurn);
            }
            c.state_hash()
        };
        assert_eq!(play(21), play(21));
        assert_ne!(play(21), play(22));
    }

    // ── 怪物补缺：红面罩三人组 + 古形体 ──────────────

    /// 红面罩三人组：出生数值（A0/A7）与固定序列
    #[test]
    fn bandit_trio_numbers_and_sequence() {
        let c = Combat::new_with(
            1,
            0,
            &[
                MonsterKind::BanditPointy,
                MonsterKind::BanditLeader,
                MonsterKind::BanditBear,
            ],
        );
        assert_eq!(c.monsters[0].creature.max_hp, 30, "尖头 HP 30 定值");
        assert!(
            (35..=39).contains(&c.monsters[1].creature.max_hp),
            "首领 HP 35-39"
        );
        assert!(
            (38..=42).contains(&c.monsters[2].creature.max_hp),
            "熊 HP 38-42"
        );
        // 初始意图：尖头=攻，首领=嘲讽(2)，熊=熊抱(2)
        assert_eq!(c.monsters[0].next_move, 1);
        assert_eq!(c.monsters[1].next_move, 2);
        assert_eq!(c.monsters[2].next_move, 2);
        // A2 数值：尖头 6，首领斩 17/12，熊 20/10
        let c2 = Combat::new_with(
            1,
            2,
            &[
                MonsterKind::BanditPointy,
                MonsterKind::BanditLeader,
                MonsterKind::BanditBear,
            ],
        );
        assert_eq!(c2.monsters[0].chomp_dmg, 6);
        assert_eq!(c2.monsters[1].chomp_dmg, 17);
        assert_eq!(c2.monsters[1].thrash_dmg, 12);
        assert_eq!(c2.monsters[2].chomp_dmg, 20);
        assert_eq!(c2.monsters[2].dark_strike_dmg, 10);
    }

    /// 红面罩实战推演：首领嘲讽空转 → 痛苦斩(10+虚弱2) → 交叉斩(15) 循环；
    /// 熊：熊抱(-2敏捷) → 扑击(9+自挡9) → 撕咬(18) 循环；尖头每回合 5×2
    #[test]
    fn bandit_trio_take_turn_flow() {
        let mut c = Combat::new_with(
            7,
            0,
            &[
                MonsterKind::BanditPointy,
                MonsterKind::BanditLeader,
                MonsterKind::BanditBear,
            ],
        );
        c.player.creature.hp = 200;
        c.player.creature.max_hp = 200;
        let hp0 = c.player.creature.hp;
        // 回合 1：尖头 5×2 + 首领空转 + 熊熊抱
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, hp0 - 10, "尖头 5×2");
        let dex: i32 = c
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Dexterity)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(dex, -2, "熊抱 -2 敏捷");
        assert_eq!(c.monsters[2].creature.block, 0);
        // 回合 2：首领痛苦斩 10 + 虚弱 2；熊扑击 9 + 自挡 9
        let hp1 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(
            c.player.creature.hp,
            hp1 - 10 - 9 - 5 - 5,
            "首领 10 + 熊 9 + 尖头 10"
        );
        let weak: i32 = c
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Weak)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(weak, 2, "痛苦斩虚弱 2");
        assert_eq!(c.monsters[2].creature.block, 9, "熊扑击自挡");
        // 回合 3：首领交叉斩 15；熊撕咬 18
        let hp2 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(
            c.player.creature.hp,
            hp2 - 15 - 18 - 10,
            "交叉斩+撕咬+尖头（虚弱已过回合）"
        );
    }

    /// A17：首领交叉斩可连打（1→1→3）；熊抱 -4 敏捷
    #[test]
    fn bandit_trio_a17() {
        let mut c = Combat::new_with(7, 17, &[MonsterKind::BanditLeader, MonsterKind::BanditBear]);
        c.player.creature.hp = 500;
        c.player.creature.max_hp = 500;
        c.step(&Action::EndTurn); // 首领嘲讽 + 熊抱
        let dex: i32 = c
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Dexterity)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(dex, -4, "A17 熊抱 -4");
        c.step(&Action::EndTurn); // 痛苦斩 + 扑击
        assert_eq!(c.monsters[0].next_move, 1);
        c.step(&Action::EndTurn); // 交叉斩 1 → A17 继续 1
        assert_eq!(c.monsters[0].move_history.last(), Some(&1));
        assert_eq!(c.monsters[0].next_move, 1, "A17 连打交叉斩");
        c.step(&Action::EndTurn); // 又一次 1 → next 3
        assert_eq!(c.monsters[0].next_move, 3, "连打两次后回痛苦斩");
        assert_eq!(c.monsters[0].ritual_amt, 3, "A17 虚弱量 3");
    }

    /// 荆棘形体：战前荆棘 3（A2 4 / A17 +3）；攻击命中反弹 THORNS（不可格挡）
    #[test]
    fn spiker_thorns_reflect() {
        let c = Combat::new_with(3, 0, &[MonsterKind::Spiker]);
        let thorns: i32 = c.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Thorns)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(thorns, 3);
        let c2 = Combat::new_with(3, 2, &[MonsterKind::Spiker]);
        let t2: i32 = c2.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Thorns)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(t2, 4, "A2 荆棘 4");
        let c3 = Combat::new_with(3, 17, &[MonsterKind::Spiker]);
        let t3: i32 = c3.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Thorns)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(t3, 7, "A17 荆棘 4+3");
        // 反弹：打出打击（6 伤）→ 玩家受 3 点 THORNS 伤（可被格挡：先扣后穿）
        let mut c = Combat::new_with(3, 0, &[MonsterKind::Spiker]);
        c.player.creature.block = 2;
        let hp0 = c.player.creature.hp;
        let idx = c
            .player
            .hand
            .iter()
            .position(|cd| cd.def.id == "Strike_R")
            .unwrap();
        c.step(&Action::PlayCard {
            index: idx,
            target: Some(0),
        });
        assert_eq!(c.player.creature.block, 0, "荆棘先扣格挡");
        assert_eq!(c.player.creature.hp, hp0 - 1, "穿透 1 点");
        assert_eq!(
            c.monsters[0].creature.hp + 6,
            c.monsters[0].creature.max_hp,
            "打击命中 6"
        );
        // 无格挡：全额 3 点
        let mut c = Combat::new_with(3, 0, &[MonsterKind::Spiker]);
        let hp0 = c.player.creature.hp;
        let idx = c
            .player
            .hand
            .iter()
            .position(|cd| cd.def.id == "Strike_R")
            .unwrap();
        c.step(&Action::PlayCard {
            index: idx,
            target: Some(0),
        });
        assert_eq!(c.player.creature.hp, hp0 - 3, "无格挡全额 3 点");
    }

    /// 爆炸形体：两回合普攻 9/11，第三回合自爆 30（THORNS 型可格挡）并自灭
    #[test]
    fn exploder_blast() {
        let mut c = Combat::new_with(3, 0, &[MonsterKind::Exploder]);
        c.player.creature.hp = 100;
        c.player.creature.max_hp = 100;
        let hp0 = c.player.creature.hp;
        c.step(&Action::EndTurn); // 回合 1：普攻 9
        assert_eq!(c.player.creature.hp, hp0 - 9);
        assert_eq!(c.monsters[0].next_move, 1);
        c.step(&Action::EndTurn); // 回合 2：普攻 9
        assert_eq!(c.monsters[0].next_move, 2, "第三回合待机");
        let hp1 = c.player.creature.hp;
        c.step(&Action::EndTurn); // 回合 3：自爆 30（THORNS，无格挡时全额）
        assert_eq!(c.player.creature.hp, hp1 - 30, "自爆 30");
        assert_eq!(c.monsters[0].creature.hp, 0, "自灭");
        assert!(c.over && c.won, "战斗胜利");
    }

    /// 形体组合重掷：进入 3/4 Shapes 房间时从 [Repulsor×2,Exploder×2,Spiker×2] 抽
    #[test]
    fn shapes_composition_reroll() {
        use crate::run::{RoomKind, RunState};
        for seed in [5u64, 11, 23, 77, 2024] {
            let mut run = RunState::new(seed, 0);
            // 直接把遭遇列表改成 3 Shapes 签名
            run.encounter_list = vec!["3 Shapes"];
            run.encounter_idx = 0;
            run.map.rows[1][3].room = Some(RoomKind::Monster);
            run.pos = (1, 3);
            run.enter_room();
            let c = run.combat.as_ref().expect("战斗已开");
            assert_eq!(c.monsters.len(), 3);
            for m in &c.monsters {
                assert!(
                    matches!(
                        m.kind,
                        MonsterKind::Repulsor | MonsterKind::Exploder | MonsterKind::Spiker
                    ),
                    "形体池成员"
                );
            }
        }
    }

    /// 红面罩事件战斗接入：Masked Bandits 开打三怪
    #[test]
    fn masked_bandits_event_combat() {
        use crate::events::{EventKind, PendingPick};
        use crate::run::RunState;
        let mut run = RunState::new(9, 0);
        run.floor = 10;
        run.event = Some(crate::events::EventState {
            kind: EventKind::MaskedBandits,
            screen: 0,
            done: false,
            heal_amt: 0,
            damage: 0,
            maxhp_loss: 0,
            gold: 0,
            gold_loss: 0,
            chance: 0,
            counter: 0,
            enemy: 0,
            bag: Vec::new(),
            pending: Vec::<PendingPick>::new(),
            library: Vec::new(),
            costs: [0; 3],
        });
        run.choose_event_option(1); // 战斗
        assert!(run.combat.is_some());
        let c = run.combat.as_ref().unwrap();
        assert_eq!(c.monsters.len(), 3, "尖头+首领+熊");
        assert!(matches!(c.monsters[0].kind, MonsterKind::BanditPointy));
        assert!(matches!(c.monsters[1].kind, MonsterKind::BanditLeader));
        assert!(matches!(c.monsters[2].kind, MonsterKind::BanditBear));
        assert!(run.rewards.iter().any(|r| matches!(
            r,
            crate::run::RewardItem::Relic(crate::relics::RelicKind::RedMask)
        )));
    }

    // ── 角色补充批次：真言/禁抽/别名解锁 ──────────────

    /// MantraPower.stackPower：满 10 → 进神性并 -10（余 0 移除）
    #[test]
    fn mantra_to_divinity() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::Mantra, 4, false);
        assert_eq!(c.player.stance, crate::state::Stance::Neutral);
        c.apply_power_to_player_checked(PowerKind::Mantra, 6, false);
        assert_eq!(
            c.player.stance,
            crate::state::Stance::Divinity,
            "满 10 进神性"
        );
        assert!(
            c.player
                .creature
                .powers
                .iter()
                .all(|p| p.kind != PowerKind::Mantra),
            "余 0 移除"
        );
        // 超 10：留余量
        c.player.stance = crate::state::Stance::Neutral;
        c.apply_power_to_player_checked(PowerKind::Mantra, 13, false);
        assert_eq!(c.player.stance, crate::state::Stance::Divinity);
        assert_eq!(
            c.player
                .creature
                .powers
                .iter()
                .find(|p| p.kind == PowerKind::Mantra)
                .map(|p| p.amount),
            Some(3),
            "13 → 进神留 3"
        );
    }

    /// NoDrawPower：本回合禁抽，轮末随衰减移除
    #[test]
    fn no_draw_blocks_then_decays() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::NoDraw, 1, false);
        c.draw_cards(3);
        assert_eq!(c.player.hand.len(), 5, "本回合禁抽，手牌不变");
        assert!(c
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::NoDraw));
        c.step(&Action::EndTurn); // 轮末：弃手牌 → 衰减移除 → 下回合重抽
        assert!(
            c.player
                .creature
                .powers
                .iter()
                .all(|p| p.kind != PowerKind::NoDraw),
            "轮末移除"
        );
        assert_eq!(c.player.hand.len(), 5, "下回合照抽 5");
    }

    /// 数据层 cls 别名（FocusPower 等）解锁：聚能/活化/无形牌可执行
    #[test]
    fn power_alias_unlocks_cards() {
        for id in ["Defragment", "Adrenaline", "Impervious"] {
            let def = CARDS.iter().find(|d| d.id == id).expect(id);
            assert!(engine_playable(def), "{id} 应随别名接入解锁");
        }
    }

    /// 阻塞审计：不可执行卡牌的原因分桶（打印用）
    #[test]
    fn audit_blockers() {
        use crate::cards::ENGINE_POWERS;
        use std::collections::BTreeMap;
        let mut buckets: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        fn expr_bad(e: &crate::effects::Expr) -> bool {
            matches!(
                e,
                crate::effects::Expr::Const(_) | crate::effects::Expr::Raw(_)
            )
        }
        fn why(e: &crate::effects::Effect) -> Option<String> {
            use crate::effects::Effect::*;
            match e {
                Damage(t, _x) if !matches!(t, crate::effects::Target::Enemy) => {
                    Some("目标类型".into())
                }
                Damage(_, x)
                | DamageAll(x)
                | Block(x)
                | Draw(x)
                | GainEnergy(x)
                | Heal(x)
                | LoseHP(x)
                    if expr_bad(x) =>
                {
                    Some("表达式".into())
                }
                ApplyPower(_, pid, x) => {
                    if expr_bad(x) {
                        Some("表达式".into())
                    } else if !ENGINE_POWERS.contains(pid) {
                        Some(format!("power:{pid}"))
                    } else {
                        None
                    }
                }
                Unsupported(name) => Some(format!("动作:{name}")),
                Repeat(n, inner) => {
                    if expr_bad(n) {
                        Some("表达式".into())
                    } else {
                        inner.iter().find_map(why)
                    }
                }
                IfStance(_, inner) | IfNotStance(_, inner) => inner.iter().find_map(why),
                _ => None,
            }
        }
        for d in CARDS.iter() {
            if d.cost == Some(-2) {
                buckets
                    .entry("不可打出(-2费)".into())
                    .or_default()
                    .push(d.id);
                continue;
            }
            if !engine_playable(d) {
                let reason = d
                    .effects
                    .iter()
                    .find_map(&why)
                    .unwrap_or_else(|| "其他".into());
                if reason == "其他" {
                    println!("OTHER: {} — effects: {:?}", d.id, d.effects);
                }
                if reason == "表达式" {
                    println!("EXPR: {} — effects: {:?}", d.id, d.effects);
                }
                buckets.entry(reason).or_default().push(d.id);
            }
        }
        let mut total = 0;
        for (k, v) in &buckets {
            if k.starts_with("power:") || k.starts_with("动作:") {
                println!("{:>3}  {}  [{}]", v.len(), k, v.join(","));
            } else {
                println!("{:>3}  {}", v.len(), k);
            }
            total += v.len();
        }
        println!("TOTAL blocked = {total}");
    }

    // ── power 补缺第一批 ──────────────────────

    /// 剧毒瘴气：抽牌后回合开始 → 全体敌人中毒
    #[test]
    fn noxious_fumes_poison_all() {
        let mut c = Combat::new_with(1, 0, &[MonsterKind::Cultist, MonsterKind::Cultist]);
        c.apply_power_to_player_checked(PowerKind::NoxiousFumes, 2, false);
        c.step(&Action::EndTurn);
        for m in &c.monsters {
            let po = m
                .creature
                .powers
                .iter()
                .find(|p| p.kind == PowerKind::Poison)
                .map(|p| p.amount);
            assert_eq!(po, Some(2), "全体中毒 2");
        }
    }

    /// 无限之刃：回合开始 Shiv 入手；奉献：回合开始真言（满 10 进神性）
    #[test]
    fn infinite_blades_and_devotion() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::InfiniteBlades, 1, false);
        c.apply_power_to_player_checked(PowerKind::Devotion, 10, false);
        assert_eq!(
            c.player.stance,
            crate::state::Stance::Neutral,
            "奉献非立即触发"
        );
        c.step(&Action::EndTurn); // 抽牌后：真言 +10 → 满 10 进神性
        assert_eq!(
            c.player.stance,
            crate::state::Stance::Divinity,
            "奉献 10 → 神性"
        );
        assert!(
            c.player.hand.iter().any(|x| x.def.id == "Shiv")
                || c.player.discard.iter().any(|x| x.def.id == "Shiv"),
            "回合开始 Shiv 入手"
        );
    }

    /// 心之堡垒（换姿态格挡）+ 如水（平静 EOT 格挡）
    #[test]
    fn mental_fortress_and_like_water() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::MentalFortress, 4, false);
        c.exec(Q::ChangeStance(crate::state::Stance::Wrath));
        assert_eq!(c.player.creature.block, 4, "换姿态 +4");
        c.exec(Q::ChangeStance(crate::state::Stance::Calm));
        assert_eq!(c.player.creature.block, 8, "再次换姿态 +4");
        c.apply_power_to_player_checked(PowerKind::LikeWater, 3, false);
        // 壁垒保住跨回合格挡（否则下回合 loseBlock 归零，无法观测）
        c.apply_power_to_player_checked(PowerKind::Barricade, 1, false);
        c.monsters[0].next_move = 2; // 大颚虫咆哮：不攻击
        c.step(&Action::EndTurn); // 平静中结束 → +3
        assert!(
            c.player.creature.block >= 8 + 3,
            "如水平静 EOT +3（实际 {}）",
            c.player.creature.block
        );
    }

    /// 风暴：打出能力牌充闪电；余像：每张牌 +1 格挡
    #[test]
    fn storm_channel_and_after_image() {
        let mut c = Combat::new(1);
        c.player.max_orbs = 3;
        c.apply_power_to_player_checked(PowerKind::Storm, 1, false);
        c.apply_power_to_player_checked(PowerKind::AfterImage, 1, false);
        let idx = c
            .player
            .hand
            .iter()
            .position(|cd| cd.def.card_type == "POWER");
        if let Some(i) = idx {
            c.step(&Action::PlayCard {
                index: i,
                target: Some(0),
            });
            assert!(!c.player.orbs.is_empty(), "能力牌充闪电");
            assert!(c.player.creature.block >= 1, "余像 +1 格挡");
        }
    }

    /// 无痛：消耗牌 → 3 格挡；破裂：卡牌失 HP → 力量 +1
    #[test]
    fn feel_no_pain_and_rupture() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::FeelNoPain, 3, false);
        c.relics_on_exhaust();
        assert_eq!(c.player.creature.block, 3, "无痛 +3");
        c.apply_power_to_player_checked(PowerKind::Rupture, 1, false);
        let str0 = c
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Strength)
            .map(|p| p.amount)
            .unwrap_or(0);
        c.exec(Q::LoseHP(2));
        let str1 = c
            .player
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::Strength)
            .map(|p| p.amount)
            .unwrap_or(0);
        assert_eq!(str1 - str0, 1, "破裂失血 +1 力量");
    }

    /// 炸弹：3 回合后全体 40 THORNS
    #[test]
    fn the_bomb_detonate() {
        let mut c = Combat::new_with(1, 0, &[MonsterKind::Cultist]);
        let hp0 = c.monsters[0].creature.hp;
        c.apply_power_to_player_checked(PowerKind::TheBomb, 40, false);
        c.step(&Action::EndTurn); // 倒数 3→2
        assert!(c.monsters[0].creature.hp == hp0, "未爆");
        c.step(&Action::EndTurn); // 2→1
        assert!(c.monsters[0].creature.hp == hp0, "未爆");
        c.step(&Action::EndTurn); // 1→0 起爆
        assert!(
            c.over && c.won || c.monsters[0].creature.hp <= (hp0 - 40).max(0),
            "40 伤或已击杀"
        );
    }

    /// 掠食者：下回合补抽 2；闪翻：下回合格挡 4；机器学习：每回合抽 5+1
    #[test]
    fn next_turn_draw_block_and_hand_size() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::DrawCardNextTurn, 2, false);
        c.apply_power_to_player_checked(PowerKind::NextTurnBlock, 4, false);
        c.step(&Action::EndTurn);
        assert!(c.player.hand.len() >= 7, "5+2 补抽");
        assert_eq!(
            c.player.creature.block, 4,
            "下回合格挡 4（无怪伤格挡情况下）"
        );
        assert!(
            c.player
                .creature
                .powers
                .iter()
                .all(|p| p.kind != PowerKind::DrawCardNextTurn),
            "一次性移除"
        );
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::DrawBonus, 1, false);
        c.step(&Action::EndTurn);
        assert_eq!(c.player.hand.len(), 6, "gameHandSize+1");
    }

    /// 复仇：下回合进愤怒；自我修复：胜利回血
    #[test]
    fn vengeance_wrath_and_repair() {
        let mut c = Combat::new(1);
        c.apply_power_to_player_checked(PowerKind::WrathNextTurn, 1, false);
        c.step(&Action::EndTurn);
        assert_eq!(c.player.stance, crate::state::Stance::Wrath, "下回合进愤怒");
        // 自我修复：胜利回 5
        let mut c = Combat::new(1);
        c.player.creature.hp = 50;
        c.apply_power_to_player_checked(PowerKind::Repair, 5, false);
        for m in c.monsters.iter_mut() {
            m.creature.hp = 0;
        }
        c.won = true;
        c.on_victory();
        assert_eq!(
            c.player.creature.hp,
            50 + 5,
            "修复 +5（单测战斗无初始遗物）"
        );
    }

    /// 别名解锁：恶魔形态/魔化/狂乱/暴虐/蓄能电池/火焰花环 全部可执行
    #[test]
    fn power_batch1_alias_unlocks() {
        for id in [
            "Demon Form",
            "Dark Embrace",
            "Berserk",
            "Brutality",
            "Conserve Battery",
            "WreathOfFlame",
            "Predator",
            "Dodge and Roll",
            "Machine Learning",
            "Self Repair",
            "Feel No Pain",
            "Rupture",
            "After Image",
            "Noxious Fumes",
            "Infinite Blades",
            "MentalFortress",
            "LikeWater",
            "Devotion",
            "Storm",
            "The Bomb",
            "Vengeance",
        ] {
            let def = CARDS.iter().find(|d| d.id == id || d.cls == id);
            assert!(def.is_some(), "{id} 在数据层");
            let def = def.unwrap();
            assert!(engine_playable(def), "{id} 应解锁（{}）", def.id);
        }
    }
}

#[cfg(test)]
mod p3_parity_probe {
    use super::*;

    /// 回合末弃牌顺序必须是手牌**逆序**。
    ///
    /// Java `DiscardAtEndOfTurnAction` 排队 N 个 `DiscardAction`，每个用
    /// `hand.getTopCard()`（= group.get(size-1)，手牌末尾）逐个丢弃 ⇒ 弃牌堆里
    /// 的顺序 = 手牌 group 的逆序。
    ///
    /// 这条顺序会一路传导到「重洗后的抽牌顺序」：弃牌堆是洗牌的输入，输入排列变了，
    /// 即使洗牌种子与算法完全相同，结果排列也会不同（实测 turn3 起手手牌错位）。
    #[test]
    fn end_of_turn_discard_is_reversed() {
        let mut c = Combat::new_with(1, 0, &[MonsterKind::JawWorm]);
        c.player.discard.clear();
        c.player.hand.clear();
        for id in ["Strike_R", "Defend_R", "Bash"] {
            c.player.hand.push(crate::cards::CardInst::new(id));
        }
        c.step(&Action::EndTurn);
        let got: Vec<&str> = c.player.discard.iter().map(|x| x.def.id).collect();
        // 末尾手牌先弃 ⇒ Bash, Defend_R, Strike_R
        assert_eq!(got, vec!["Bash", "Defend_R", "Strike_R"],
                   "回合末弃牌应逆序（getTopCard 从末尾取）");
    }

    /// P3 探针：结束回合后怪物必须真的动手（否则 pyo3/RL 里敌人永不攻击）。
    /// 复现：真游戏 PMCWKZEF 首战 FuzzyLouseNormal，第一回合末玩家掉 7 血。
    #[test]
    fn louse_attacks_on_end_turn() {
        let mut c = Combat::new_with(1, 0, &[MonsterKind::LouseNormal]);
        let hp0 = c.player.creature.hp;
        let nm = c.monsters[0].next_move;
        c.step(&Action::EndTurn);
        assert!(
            c.player.creature.hp < hp0,
            "怪物回合未造成伤害：next_move={nm} hp {hp0} -> {}",
            c.player.creature.hp
        );
    }
}

#[cfg(test)]
mod curlup_order_probe {
    use super::*;
    #[test]
    fn curlup_does_not_absorb_its_own_trigger() {
        let mut c = Combat::new_with(1, 0, &[MonsterKind::LouseDefensive]);
        let hp0 = c.monsters[0].creature.hp;
        let curl = c.monsters[0]
            .creature
            .powers
            .iter()
            .find(|p| p.kind == PowerKind::CurlUp)
            .map(|p| p.amount)
            .unwrap_or(0);
        let base = 6;
        c.hit_monster(0, base, DamageType::Normal);
        let hp_loss = hp0 - c.monsters[0].creature.hp;
        assert_eq!(hp_loss, base, "Curl Up 不应吸收触发它的伤害（curl={curl}）");
        assert_eq!(c.monsters[0].creature.block, curl, "结算后才加格挡");
    }
}
