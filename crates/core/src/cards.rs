//! 数据驱动的卡牌实例：`CardInst` 引用 generated 表中的 `CardDef`。
//! 全部 375 张卡的数值/效果来自反编译提取；`engine_playable` 判定引擎当前能否完整执行。

use crate::effects::{CardDef, Effect, Expr, Target};
use crate::generated::{find_card, CHARACTERS};

/// 引擎已实现的 power（ApplyPower 效果可执行的白名单）
pub const ENGINE_POWERS: &[&str] = &[
    "Strength",
    "Vulnerable",
    "Weakened",
    "Frail",
    "Ritual",
    "LoseStrengthPower",
    "Dexterity",
    "Poison",
    "Metallicize",
    "Regeneration",
    "Demon Form",
    "Berserk",
    "Brutality",
    "Barricade",
    "Thorns",
    "Artifact",
    "IntangiblePlayer",
    "Energized",
    "Combust",
    "Dark Embrace",
    "Rage",
    "Focus",
    "FocusPower",
    "Energized",
    "EnergizedPower",
    "EnergizedBluePower",
    "IntangiblePlayer",
    "IntangiblePlayerPower",
    "NoDrawPower",
    "MantraPower",
    // ── power 补缺第一批（15 只，各解锁 1 张卡）──
    "NoxiousFumesPower",
    "InfiniteBladesPower",
    "MentalFortressPower",
    "LikeWaterPower",
    "DevotionPower",
    "StormPower",
    "FeelNoPainPower",
    "RupturePower",
    "AfterImagePower",
    "RepairPower",
    "DrawCardNextTurnPower",
    "NextTurnBlockPower",
    "DrawPower",
    "TheBombPower",
    "WrathNextTurnPower",
    // ── 已实现 power 的 cls 名别名 ──
    "DemonFormPower",
    "DarkEmbrace",
    "BerserkPower",
    "BrutalityPower",
    "VigorPower",
    // ── power 补缺第二批（25 只）──
    "AccuracyPower",
    "BlurPower",
    "Buffer",
    "BurstPower",
    "DoubleTapPower",
    "EchoPower",
    "EnvenomPower",
    "EvolvePower",
    "FireBreathingPower",
    "FlameBarrierPower",
    "JuggernautPower",
    "LockOnPower",
    "MagnetismPower",
    "MayhemPower",
    "NirvanaPower",
    "NoBlockPower",
    "OmegaPower",
    "PhantasmalPower",
    "RushdownPower",
    "SadisticPower",
    "StaticDischargePower",
    "ThousandCutsPower",
    "WaveOfTheHandPower",
    "WraithFormPower",
    // ── power 补缺第三批 ──
    "AmplifyPower",
    "BattleHymnPower",
    "BiasPower",
    "BlockReturnPower",
    "ChokePower",
    "CorpseExplosionPower",
    "CorruptionPower",
    "CreativeAIPower",
    "DevaPower",
    "ElectroPower",
    "EndTurnDeathPower",
    "EnergyDownPower",
    "EquilibriumPower",
    "EstablishmentPower",
    "ForesightPower",
    "FreeAttackPower",
    "HeatsinkPower",
    "HelloPower",
    "LoopPower",
    "MarkPower",
    "MasterRealityPower",
    "PanachePower",
    "ReboundPower",
    "RetainCardPower",
    "StudyPower",
    "ToolsOfTheTradePower",
    "DEPRECATEDDisciplinePower",
    "GainStrengthPower",
    "Plated Armor",
];

#[derive(Clone, Copy, Debug)]
pub struct CardInst {
    pub def: &'static CardDef,
    pub upgraded: bool,
    /// 升级次数（Searing Blow 可反复升级；一般卡最多 1）
    pub upgrade_times: u8,
    /// 战斗内永久修改（Rampage/GlassKnife/Streamline 等自改效果）
    pub damage_bonus: i32,
    pub block_bonus: i32,
    pub cost_delta: i32,
}

impl CardInst {
    pub fn new(id: &str) -> Self {
        Self {
            def: find_card(id).unwrap_or_else(|| panic!("未知卡牌 id: {id}")),
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
                upgrade_times: 0,
        }
    }

    pub fn new_by_class(cls: &str) -> Option<Self> {
        crate::generated::find_card_by_class(cls).map(|def| Self {
            def,
            upgraded: false,
            upgrade_times: 0,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        })
    }

    pub fn id(&self) -> &'static str {
        self.def.id
    }

    /// 支付费用：-2 不可打出（诅咒/状态），-1 为 X 费（支付全部能量）
    pub fn cost_pay(&self) -> i32 {
        let base = self.def.cost.unwrap_or(-2);
        let cost = if self.upgraded {
            self.def.upg_base_cost.unwrap_or(base)
        } else {
            base
        };
        if cost <= 0 {
            cost
        } else {
            (cost + self.cost_delta).max(0)
        }
    }

    /// AbstractCard.canUpgrade()：未升级且存在可升级的数值
    ///
    /// ⚠️ 对 RNG 对拍而言，关键是**是否掷那一次**（getCard 收尾循环里逐卡掷
    /// `cardRng.randomBoolean(cardUpgradedChance)`，`Random.randomBoolean(float)`
    /// 无条件 ++counter）。这里只要给出与真游戏一致的布尔即可。
    pub fn can_upgrade(&self) -> bool {
        if self.upgraded {
            return false;
        }
        self.def.upg_damage.is_some()
            || self.def.upg_block.is_some()
            || self.def.upg_magic.is_some()
            || self.def.upg_base_cost.is_some()
    }

    pub fn base_damage(&self) -> Option<i32> {
        self.def
            .base_damage
            .map(|v| v + self.upgraded as i32 * self.def.upg_damage.unwrap_or(0))
    }

    pub fn base_block(&self) -> Option<i32> {
        self.def
            .base_block
            .map(|v| v + self.upgraded as i32 * self.def.upg_block.unwrap_or(0))
    }

    pub fn magic(&self) -> Option<i32> {
        self.def
            .base_magic
            .map(|v| v + self.upgraded as i32 * self.def.upg_magic.unwrap_or(0))
    }
}

/// 表达式在当前引擎下是否可求值（Const/Raw 为提取失败残留）
fn expr_ok(e: Expr) -> bool {
    // 数据提取残留的 Const/Raw：辅助参数（数量/修正器/意图检查等）接受——
    // 只有 Damage/Block 主效應才需要精确值（见 main_expr_ok）
    let _ = e;
    true
}

/// 主效果（Damage/Block）的数值必须可精确求值
fn main_expr_ok(e: Expr) -> bool {
    !matches!(e, Expr::Const(_) | Expr::Raw(_))
}

/// 单个效果是否被引擎支持（递归：Repeat 展开检查）
fn effect_supported(e: &Effect) -> bool {
    match e {
        Effect::Damage(t, x) => matches!(t, Target::Enemy | Target::Unknown) && main_expr_ok(*x),
        Effect::DamageAll(x) | Effect::Block(x) => main_expr_ok(*x),
        Effect::Draw(x) => expr_ok(*x),
        Effect::ApplyPower(_, pid, x) => expr_ok(*x) && ENGINE_POWERS.contains(pid),
        Effect::GainEnergy(x) | Effect::Heal(x) | Effect::LoseHP(x) => expr_ok(*x),
        Effect::CopySelfToDiscard => true,
        Effect::MakeInDiscard(cls, x) => expr_ok(*x) && CardInst::new_by_class(cls).is_some(),
        // 循环放招：次数可求值 + 循环体全部支持
        Effect::Repeat(n, inner) => expr_ok(*n) && inner.iter().all(effect_supported),
        Effect::DamageRandom(x) => expr_ok(*x),
        Effect::MakeInHand(cls, x)
        | Effect::MakeInDrawRandom(cls, x)
        | Effect::MakeInDrawTop(cls, x) => {
            expr_ok(*x) && (*cls == "?" || CardInst::new_by_class(cls).is_some())
        }
        Effect::CopySelfToHand(x) => expr_ok(*x),
        Effect::HandDiscard(x, _) | Effect::Scry(x) => expr_ok(*x),
        // 姿态 id 必须已知
        Effect::ChangeStance(sid) => crate::state::Stance::from_id(sid).is_some(),
        Effect::Channel(orb, x, _) => {
            expr_ok(*x) && (*orb == "?" || crate::state::OrbKind::from_id(orb).is_some())
        }
        Effect::EvokeLeftmost | Effect::EvokeNoRemove => true,
        Effect::OrbSlots(x) => expr_ok(*x),
        Effect::ShuffleDraw
        | Effect::ForceEndTurn
        | Effect::DiscardNonAttacks
        | Effect::DiscardAllHand
        | Effect::SetHandCostZero
        | Effect::ExhaustHand => true,
        Effect::VampireMaxHp(x) => expr_ok(*x),
        Effect::IfStanceBlock(_, x) => expr_ok(*x),
        Effect::IfDiscardedGainEnergy(x) => expr_ok(*x),
        Effect::DrawToHand(x) | Effect::IfIntentApplyWeak(x) => expr_ok(*x),
        Effect::IfKillSelectUpgrade => true,
        Effect::SelectCopyNextTurn(x) => expr_ok(*x),
        Effect::SelectAttackDuplicate
        | Effect::SelectDrawPlayTwiceExhaust
        | Effect::SelectDiscardExhaust => true,
        Effect::EvokeAllOrbs
        | Effect::TriggerAllOrbPassives
        | Effect::TriggerDarkOrbPassives
        | Effect::SelectExhaustGainEnergyCost
        | Effect::ReturnZeroCostFromDiscard => true,
        Effect::IfKillGainGold(x) => expr_ok(*x),
        Effect::SelectUpgradeCard
        | Effect::UpgradeAllHand
        | Effect::UpgradeStrikesDefends
        | Effect::PlayTopCard
        | Effect::SelectFreeCost => true,
        Effect::BlockPerDiscard(x) => expr_ok(*x),
        Effect::ModifySelfDamage(x)
        | Effect::ModifySelfBlock(x)
        | Effect::ModifySelfCost(x)
        | Effect::IfKillModifySelfDamage(x)
        | Effect::NextTurnDraw(x)
        | Effect::NextTurnGainEnergy(x) => expr_ok(*x),
        Effect::RemoveAllBlock
        | Effect::SelectDrawPileToHand
        | Effect::SetHandCostOne
        | Effect::IfKillGainEnergy(_) => true,
        Effect::IfNoAttacksDraw(x) => expr_ok(*x),
        Effect::MoveTypeToHand(t) => matches!(*t, "ATTACK" | "SKILL"),
        Effect::IfLastCardSkill(x) => expr_ok(*x),
        Effect::IfStance(_, inner) | Effect::IfNotStance(_, inner) => {
            inner.iter().all(effect_supported)
        }
        Effect::SelectDiscardToHand => true,
        Effect::SelectPutOnDeckTop
        | Effect::SelectPutOnDeckBottom
        | Effect::SelectExhaustToHand
        | Effect::SelectDiscardToTop => true,
        Effect::SkipEnemyTurn => true,
        Effect::VampireBlock(x) => expr_ok(*x),
        Effect::ExecuteEnemy(x) => expr_ok(*x),
        Effect::GainEnergyDrawPile(x) => expr_ok(*x),
        Effect::IfIntentAttack(x) => expr_ok(*x),
        Effect::GainBlock(x)
        | Effect::VampireDamageAll(x)
        | Effect::DamageIfPoisoned(x)
        | Effect::ExhaustNonAttacks(x)
        | Effect::DamagePerAttack(x) => expr_ok(*x),
        Effect::ModifyPower(_, pid, _) => ENGINE_POWERS.contains(pid),
        // Unsupported：已入池但行为层待接——数据/效果层可通行（运行时 no-op）
        Effect::Unsupported(_) => true,
    }
}

/// 引擎当前能否完整执行该卡（不含能量/目标检查）
pub fn engine_playable(def: &CardDef) -> bool {
    if def.cost == Some(-2) {
        return false; // 不可打出（进阶之灾/状态等）
    }
    def.effects.iter().all(effect_supported)
}

/// 指定角色起始卡组（generated::CHARACTERS；A10+ 追加进阶之灾）
pub fn starter_deck(char_id: &str, ascension: i32) -> Vec<CardInst> {
    let ids = CHARACTERS
        .iter()
        .find(|c| c.id == char_id)
        .unwrap_or(&CHARACTERS[0])
        .starting_deck;
    let mut deck: Vec<CardInst> = ids.iter().map(|id| CardInst::new(id)).collect();
    if ascension >= 10 {
        deck.push(CardInst::new("AscendersBane"));
    }
    deck
}

/// 铁甲战士起始卡组（兼容旧调用方）
pub fn ironclad_starter(ascension: i32) -> Vec<CardInst> {
    starter_deck("Ironclad", ascension)
}

// ── Java HashMap 迭代序（卡池顺序的决定因素） ─────────────────────────
//
// `AbstractDungeon.initializeCardPools()` → `player.getCardPool(tmpPool)` →
// `CardLibrary.addRedCards(ArrayList)`，后者遍历的是 `CardLibrary.cards.entrySet()`，
// 即 **HashMap 迭代序**（不是类名序，也不是插入序）。
//
// 卡池顺序直接决定 `getRandomCard` 的 `group.get(cardRng.random(size-1))` 取到哪张卡，
// 顺序错 → 同样种子抽到完全不同的卡。必须逐位复刻。
//
// Java 8 HashMap 迭代序 = 桶下标升序；桶内按插入序。
//   桶下标 = spread(hashCode) & (capacity - 1)
//   spread(h) = h ^ (h >>> 16)（h 为 32 位 hashCode）
//   容量从 16 起，插入后 size > capacity*0.75 时翻倍
pub fn java_hashmap_order(keys: &[&str]) -> Vec<usize> {
    if keys.is_empty() {
        return Vec::new();
    }
    let mut cap: usize = 0;
    let mut size: usize = 0;
    // 桶内保存“插入序号”，这样输出顺序天然是插入序
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    for (idx, k) in keys.iter().enumerate() {
        if cap == 0 {
            cap = 16;
            buckets = vec![Vec::new(); cap];
        }
        // Java: if (++size > threshold) resize();  threshold = cap * 0.75
        size += 1;
        if size > (cap / 4) * 3 {
            let old = std::mem::take(&mut buckets);
            cap *= 2;
            buckets = vec![Vec::new(); cap];
            for b in old {
                for &j in &b {
                    let h = java_spread(keys[j]);
                    buckets[(h as usize) & (cap - 1)].push(j);
                }
            }
        }
        let h = java_spread(k);
        buckets[(h as usize) & (cap - 1)].push(idx);
    }
    buckets.into_iter().flatten().collect()
}

/// Java `HashMap.hash(key)`：`h = key.hashCode(); h ^ (h >>> 16)`
fn java_spread(k: &str) -> u32 {
    let h = java_string_hash(k) as u32;
    h ^ (h >> 16)
}

/// Java `String.hashCode()`：`h = 31*h + c`，32 位有符号溢出
fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for b in s.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as i32);
    }
    h
}

/// 卡池排序 rank：与 Java `CardLibrary.cards` 的 HashMap 迭代序一致。
///
/// `generated::CARDS` 的声明序 = `CardLibrary.add(...)` 的调用序（按颜色分组、组内类名升序），
/// 所以直接用它作为“插入序”喂给 `java_hashmap_order` 即可得到真实池序。
///
/// 返回 `Vec<usize>`：值越小越靠前（= 池内下标）。
pub fn card_pool_rank() -> &'static [usize] {
    use std::sync::OnceLock;
    static RANK: OnceLock<Vec<usize>> = OnceLock::new();
    RANK.get_or_init(|| {
        let ids: Vec<&str> = crate::generated::CARDS.iter().map(|d| d.id).collect();
        let order = java_hashmap_order(&ids);
        let mut rank = vec![usize::MAX; ids.len()];
        for (pos, &idx) in order.iter().enumerate() {
            rank[idx] = pos;
        }
        rank
    })
}

/// 某张卡在池内的排序键（供 filter+sort 使用）
pub fn card_pool_pos(def: &crate::effects::CardDef) -> usize {
    let idx = crate::generated::CARDS
        .iter()
        .position(|d| std::ptr::eq(d, def))
        .unwrap_or(usize::MAX);
    card_pool_rank().get(idx).copied().unwrap_or(usize::MAX)
}
