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
