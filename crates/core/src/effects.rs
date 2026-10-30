//! 全量数据定义层：类型 + 生成表（generated/ 由 tools/gen_rust.py 产出）。
//!
//! 设计：数值/效果全部来自反编译源码的结构化提取（tools/extract_*.py），
//! 生成 Rust 静态表（零依赖、编译期检查）。效果 DSL 未覆盖的卡标记
//! `Effect::Unsupported`，手移植清单见 data/cards_full.json 的 stats/unsupported。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Enemy,
    AllEnemies,
    SelfC,
    RandomEnemy,
    Unknown,
}

/// 效果参数表达式（对应 use() 源码中的实参形态）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expr {
    N(i32),
    /// this.magicNumber
    Magic,
    /// this.damage（已过 calculateCardDamage 管线）
    DamageBase,
    /// this.block（已过 applyPowersToBlock 管线）
    BlockBase,
    /// p.currentBlock（Body Slam 类动态值）
    CurrentBlock,
    /// X 费能量
    XCost,
    /// 源码常量名（数值在提取表已解析时不再出现）
    /// 当前手牌数（运行时求值）
    HandSize,
    /// 当前抽牌堆数（运行时求值）
    DrawPileSize,
    /// 当前能量（运行时求值）
    CurrentEnergy,
    /// 已填充充能球数（运行时求值）
    OrbCount,
    /// 弃牌堆数（运行时求值）
    DiscardPileSize,
    Const(&'static str),
    /// 未解析表达式（待手移植）
    Raw(&'static str),
}

/// 卡牌效果 DSL（use() 入队动作的线性序列；分支卡见 status="parsed_branch"）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Damage(Target, Expr),
    DamageAll(Expr),
    Block(Expr),
    Draw(Expr),
    ApplyPower(Target, &'static str, Expr),
    GainEnergy(Expr),
    Heal(Expr),
    LoseHP(Expr),
    CopySelfToDiscard,
    MakeInDiscard(&'static str, Expr),
    ExhaustHand,
    /// 循环放招（for (i < N)：N 为次数表达式，inner 为循环体效果）
    Repeat(Expr, &'static [Effect]),
    /// 随机敌人伤害（AttackDamageRandomEnemyAction：cardRandomRng 选目标，可对拍复刻）
    DamageRandom(Expr),
    /// 生成临时牌入手牌（MakeTempCardInHandAction：手牌满则溢出弃牌堆）
    MakeInHand(&'static str, Expr),
    CopySelfToHand(Expr),
    /// 生成牌入抽牌堆：随机位置（CardGroup.addToRandomSpot）/ 置顶
    MakeInDrawRandom(&'static str, Expr),
    MakeInDrawTop(&'static str, Expr),
    /// 弃牌选择（DiscardAction：isRandom=true 时走 cardRandomRng 自动弃，无选择屏）
    HandDiscard(Expr, bool),
    /// 占卜 N（ScryAction：看抽牌堆顶 N 张，选择弃掉子集，其余保持原序置顶）
    Scry(Expr),
    /// 切换姿态（ChangeStanceAction：退出平静+2能量；同姿态无操作）
    ChangeStance(&'static str),
    /// 充能球（ChannelAction：槽满 autoEvoke=true 引出最左腾位；false 则不填）
    Channel(&'static str, Expr, bool),
    /// 引出最左球（EvokeOrbAction：触发 onEvoke + 移除 + 左移）
    EvokeLeftmost,
    /// 引出但不移除（EvokeWithoutRemovingOrbAction：仅触发 onEvoke）
    EvokeNoRemove,
    /// 增减槽位（IncreaseMaxOrbAction / DecreaseMaxOrbAction）
    OrbSlots(Expr),
    ShuffleDraw,
    /// 直接获得格挡，不走管线（DoubleYourBlockAction/Entrench）
    GainBlock(Expr),
    /// 强制结束回合（PressEndTurnButtonAction/Conclude/Meditate/Vault）
    ForceEndTurn,
    /// 全场吸血伤害（VampireDamageAllEnemiesAction/Reaper）
    VampireDamageAll(Expr),
    /// 倍增已有 power 层数（DoublePoison/LimitBreak 等）
    ModifyPower(Target, &'static str, i32),
    /// 目标有中毒时造成伤害（BaneAction）
    DamageIfPoisoned(Expr),
    /// 消耗所有非攻击手牌，每张获得 block 格挡（BlockPerNonAttackAction/Second Wind）
    ExhaustNonAttacks(Expr),
    /// 按本回合已打攻击牌数 × N 次伤害（DamagePerAttackPlayedAction/Finisher）
    DamagePerAttack(Expr),
    /// 目标意图为攻击时获得力量（SpotWeaknessAction）
    IfIntentAttack(Expr),
    /// 弃掉所有非攻击手牌（UnloadAction/Unload）
    DiscardNonAttacks,
    /// 弃掉全部手牌（BladeFury/CalculatedGamble/重复弃）
    DiscardAllHand,
    /// 本回合手牌全部免费（BulletTime：setCostForTurn(-9)）
    SetHandCostZero,
    /// 伤害 + 击杀时增加最大生命（FeedAction）
    VampireMaxHp(Expr),
    /// 在指定姿态时获得额外格挡（HaltAction：Wrath 时+9）
    IfStanceBlock(&'static str, Expr),
    /// 跳过怪物回合（Vault：skipMonsterTurn=true）
    SkipEnemyTurn,
    /// 伤害+获得格挡=实际伤害（WallopAction）
    VampireBlock(Expr),
    /// 目标HP≤阈值时立即击杀（JudgementAction：处决）
    ExecuteEnemy(Expr),
    /// 抽牌堆数÷N 的能量（AggregateEnergyAction）
    GainEnergyDrawPile(Expr),
    /// 本回合弃过牌时获得能量（GainEnergyIfDiscardAction/Underhanded Strike）
    IfDiscardedGainEnergy(Expr),
    /// 选择手牌放抽牌堆顶（PutOnDeckAction/Warcry）
    SelectPutOnDeckTop,
    /// 选择手牌放抽牌堆底（SetupAction/Thinking Ahead）
    SelectPutOnDeckBottom,
    /// 从消耗堆选牌入手（ExhumeAction/Exhume）
    SelectExhaustToHand,
    /// 从弃牌堆选牌放抽牌堆顶（DiscardPileToTopOfDeckAction/Headbutt）
    SelectDiscardToTop,
    /// 抽到手牌有 N 张（ExpertiseAction：draw until hand.size() == N）
    DrawToHand(Expr),
    /// 目标意图含攻击时施加虚弱（ForTheEyesAction）
    IfIntentApplyWeak(Expr),
    /// 从弃牌堆选牌入手（BetterDiscardPileToHandAction/Hologram）
    SelectDiscardToHand,
    /// 移除目标全部格挡（RemoveAllBlockAction/Melter）
    RemoveAllBlock,
    /// 在指定姿态时执行效果（Indignation/InnerPeace 条件分支）
    IfStance(&'static str, &'static [Effect]),
    /// 不在指定姿态时执行效果
    IfNotStance(&'static str, &'static [Effect]),
    /// 从抽牌堆选牌入手（BetterDrawPileToHandAction/Seek）
    SelectDrawPileToHand,
    /// 上一张牌是技能时抽牌（SanctityAction）
    IfLastCardSkill(Expr),
    /// 手牌费用降为 1（Enlightenment：cost > 1 → cost = 1）
    SetHandCostOne,
    /// 手牌无攻击牌时抽牌（ConditionalDrawAction/Impatience）
    IfNoAttacksDraw(Expr),
    /// 击杀时获得能量（SunderAction 的能量部分）
    IfKillGainEnergy(Expr),
    /// 将抽牌堆中指定类型全部移入手（DrawPileToHand/Violence）
    MoveTypeToHand(&'static str),
    /// 永久修改本卡伤害（Rampage/GlassKnife/Gash/GeneticAlgorithm/Steam/Streamline）
    ModifySelfDamage(Expr),
    ModifySelfBlock(Expr),
    ModifySelfCost(Expr),
    /// 击杀时永久修改本卡伤害（RitualDagger）
    IfKillModifySelfDamage(Expr),
    /// 下回合抽牌（Doppelganger/Collect 延迟效果）
    NextTurnDraw(Expr),
    NextTurnGainEnergy(Expr),
    /// 选择手牌升级（ArmamentsAction 基础版）
    SelectUpgradeCard,
    /// 升级全部手牌（ArmamentsAction 升级版）
    UpgradeAllHand,
    /// 升级全部打击/防御（ApotheosisAction：所有牌堆）
    UpgradeStrikesDefends,
    /// 弃牌堆数 × N 的格挡（ReinforcedBodyAction）
    BlockPerDiscard(Expr),
    /// 从抽牌堆顶打出一张卡（PlayTopCardAction/Havoc）
    PlayTopCard,
    /// 选择手牌免费化（MadnessAction：cost_delta 设为使费用为0）
    SelectFreeCost,
    /// 未支持动作（值为 Java 动作类名）——手移植清单
    Unsupported(&'static str),
}

/// 卡牌定义（数值来自 cards.json 提取表）
#[derive(Debug)]
pub struct CardDef {
    pub id: &'static str,
    pub cls: &'static str,
    pub color: &'static str,
    pub card_type: &'static str,
    pub rarity: &'static str,
    pub target: &'static str,
    pub cost: Option<i32>,
    pub base_damage: Option<i32>,
    pub base_block: Option<i32>,
    pub base_magic: Option<i32>,
    pub upg_damage: Option<i32>,
    pub upg_block: Option<i32>,
    pub upg_magic: Option<i32>,
    pub upg_base_cost: Option<i32>,
    pub flags: &'static [&'static str],
    /// parsed / parsed_branch / unsupported / no_use
    pub status: &'static str,
    pub effects: &'static [Effect],
}

/// 怪物数值定义（行为层 MonsterState 按引擎支持度增量接入，数据层全量）
#[derive(Debug)]
pub struct MonsterDef {
    pub id: &'static str,
    pub cls: &'static str,
    pub hp: &'static [HpTier],
    pub moves: &'static [MoveDef],
    /// DamageInfo 序列（数字或源码字段名）
    pub damage_infos: &'static [&'static str],
    pub ai_probabilities: &'static [f32],
    pub take_turn_actions: &'static [(u8, &'static [&'static str])],
}

#[derive(Debug, Clone, Copy)]
pub struct HpTier {
    pub asc: i32,
    pub min: Option<i32>,
    pub max: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
pub struct MoveDef {
    pub byte: u8,
    pub intent: &'static str,
    pub base: Option<i32>,
    pub damage_from_index: Option<i32>,
}

/// 角色定义
#[derive(Debug)]
pub struct CharacterDef {
    pub id: &'static str,
    pub max_hp: Option<i32>,
    pub starting_deck: &'static [&'static str],
    pub starting_relics: &'static [&'static str],
}
