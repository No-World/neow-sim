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
];

#[derive(Clone, Copy, Debug)]
pub struct CardInst {
    pub def: &'static CardDef,
    pub upgraded: bool,
}

impl CardInst {
    pub fn new(id: &str) -> Self {
        Self {
            def: find_card(id).unwrap_or_else(|| panic!("未知卡牌 id: {id}")),
            upgraded: false,
        }
    }

    pub fn new_by_class(cls: &str) -> Option<Self> {
        crate::generated::find_card_by_class(cls).map(|def| Self {
            def,
            upgraded: false,
        })
    }

    pub fn id(&self) -> &'static str {
        self.def.id
    }

    /// 支付费用：-2 不可打出（诅咒/状态），-1 为 X 费（支付全部能量）
    pub fn cost_pay(&self) -> i32 {
        let base = self.def.cost.unwrap_or(-2);
        if self.upgraded {
            if let Some(c) = self.def.upg_base_cost {
                return c;
            }
        }
        base
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
    !matches!(e, Expr::Const(_) | Expr::Raw(_))
}

/// 单个效果是否被引擎支持（递归：Repeat 展开检查）
fn effect_supported(e: &Effect) -> bool {
    match e {
        Effect::Damage(t, x) => matches!(t, Target::Enemy) && expr_ok(*x),
        Effect::DamageAll(x) | Effect::Block(x) | Effect::Draw(x) => expr_ok(*x),
        Effect::ApplyPower(_, pid, x) => expr_ok(*x) && ENGINE_POWERS.contains(pid),
        Effect::GainEnergy(x) | Effect::Heal(x) | Effect::LoseHP(x) => expr_ok(*x),
        Effect::CopySelfToDiscard => true,
        Effect::MakeInDiscard(cls, x) => expr_ok(*x) && CardInst::new_by_class(cls).is_some(),
        // 循环放招：次数可求值 + 循环体全部支持
        Effect::Repeat(n, inner) => expr_ok(*n) && inner.iter().all(effect_supported),
        Effect::DamageRandom(x) => expr_ok(*x),
        Effect::MakeInHand(cls, x)
        | Effect::MakeInDrawRandom(cls, x)
        | Effect::MakeInDrawTop(cls, x) => expr_ok(*x) && CardInst::new_by_class(cls).is_some(),
        Effect::CopySelfToHand(x) => expr_ok(*x),
        Effect::HandDiscard(x, _) | Effect::Scry(x) => expr_ok(*x),
        // 姿态 id 必须已知
        Effect::ChangeStance(sid) => crate::state::Stance::from_id(sid).is_some(),
        Effect::Channel(orb, x, _) => expr_ok(*x) && crate::state::OrbKind::from_id(orb).is_some(),
        Effect::EvokeLeftmost | Effect::EvokeNoRemove => true,
        Effect::OrbSlots(x) => expr_ok(*x),
        Effect::ShuffleDraw => true,
        // ExhaustHand（ExhaustAction 弃牌语义待核）与其余未知动作暂不可玩
        Effect::ExhaustHand | Effect::Unsupported(_) => false,
    }
}

/// 引擎当前能否完整执行该卡（不含能量/目标检查）
pub fn engine_playable(def: &CardDef) -> bool {
    if def.cost == Some(-2) {
        return false; // 不可打出（进阶之灾/状态等）
    }
    def.effects.iter().all(effect_supported)
}

/// 铁甲战士起始卡组（来自 generated::CHARACTERS；A10+ 追加进阶之灾）
pub fn ironclad_starter(ascension: i32) -> Vec<CardInst> {
    let ids = CHARACTERS
        .iter()
        .find(|c| c.id == "Ironclad")
        .expect("Ironclad")
        .starting_deck;
    let mut deck: Vec<CardInst> = ids.iter().map(|id| CardInst::new(id)).collect();
    if ascension >= 10 {
        deck.push(CardInst::new("AscendersBane"));
    }
    deck
}
