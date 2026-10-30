//! 药水系统 —— 逐只对照反编译源码（potions/*.java + helpers/PotionHelper.java）。
//!
//! 数值来源：各药水 `getPotency(ascension)` 全部为常量（无进阶缩放）；
//! 掉落池：`PotionHelper.getPotions(class, false)` = 角色专属 3 + 全角色公共表；
//! 随机：`AbstractDungeon.returnRandomPotion(limited)` = potionRng.random(0,99) 定稀有度
//! （COMMON <65 / UNCOMMON <90 / RARE ≥90，PotionHelper:61-62）→ 拒绝采样到该稀有度
//! （每次采样消耗 potionRng.random(size-1)）。
//!
//! 引擎定位：战斗内使用/丢弃管线（Action::UsePotion / DiscardPotion）；
//! 掉落获取（战斗奖励/事件/商店）属 M3 局外层，这里只提供确定性采样原语。

use crate::rng::StsRandom;

/// 药水稀有度（掉落权重见 PotionHelper.POTION_*_CHANCE）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PotionRarity {
    Common,
    Uncommon,
    Rare,
}

/// 43 只可获取药水 + 占位（PotionSlot 为商店空栏位占位，M3 用）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PotionKind {
    Fire,
    Block,
    Swift,
    Energy,
    Explosive,
    Strength,
    Weaken,
    Fear,
    Regen,
    Ancient,
    Blood,
    Dexterity,
    Steroid,
    Speed,
    BlessingOfTheForge,
    Attack,
    Skill,
    Power,
    Colorless,
    Cultist,
    FruitJuice,
    SneckoOil,
    Fairy,
    SmokeBomb,
    EntropicBrew,
    LiquidBronze,
    EssenceOfSteel,
    GamblersBrew,
    Duplication,
    LiquidMemories,
    DistilledChaos,
    Cunning,
    EssenceOfDarkness,
    Focus,
    Capacity,
    GhostInAJar,
    HeartOfIron,
    BottledMiracle,
    Stance,
    Ambrosia,
    Poison,
    Elixir,
}

/// 全部可执行药水（PotionSlot 占位除外）
pub const ALL_POTIONS: &[PotionKind] = &[
    PotionKind::Fire,
    PotionKind::Block,
    PotionKind::Swift,
    PotionKind::Energy,
    PotionKind::Explosive,
    PotionKind::Strength,
    PotionKind::Weaken,
    PotionKind::Fear,
    PotionKind::Regen,
    PotionKind::Ancient,
    PotionKind::Blood,
    PotionKind::Dexterity,
    PotionKind::Steroid,
    PotionKind::Speed,
    PotionKind::BlessingOfTheForge,
    PotionKind::Attack,
    PotionKind::Skill,
    PotionKind::Power,
    PotionKind::Colorless,
    PotionKind::Cultist,
    PotionKind::FruitJuice,
    PotionKind::SneckoOil,
    PotionKind::Fairy,
    PotionKind::SmokeBomb,
    PotionKind::EntropicBrew,
    PotionKind::LiquidBronze,
    PotionKind::EssenceOfSteel,
    PotionKind::GamblersBrew,
    PotionKind::Duplication,
    PotionKind::LiquidMemories,
    PotionKind::DistilledChaos,
    PotionKind::Cunning,
    PotionKind::EssenceOfDarkness,
    PotionKind::Focus,
    PotionKind::Capacity,
    PotionKind::GhostInAJar,
    PotionKind::HeartOfIron,
    PotionKind::BottledMiracle,
    PotionKind::Stance,
    PotionKind::Ambrosia,
    PotionKind::Poison,
    PotionKind::Elixir,
];

impl PotionKind {
    /// PotionHelper 池内 ID（掉落池以此为准）
    pub fn pool_id(&self) -> &'static str {
        match self {
            PotionKind::Fire => "Fire Potion",
            PotionKind::Block => "Block Potion",
            PotionKind::Swift => "Swift Potion",
            PotionKind::Energy => "Energy Potion",
            PotionKind::Explosive => "Explosive Potion",
            PotionKind::Strength => "Strength Potion",
            PotionKind::Weaken => "Weak Potion",
            PotionKind::Fear => "FearPotion",
            PotionKind::Regen => "Regen Potion",
            PotionKind::Ancient => "Ancient Potion",
            PotionKind::Blood => "BloodPotion",
            PotionKind::Dexterity => "Dexterity Potion",
            PotionKind::Steroid => "SteroidPotion",
            PotionKind::Speed => "SpeedPotion",
            PotionKind::BlessingOfTheForge => "BlessingOfTheForge",
            PotionKind::Attack => "AttackPotion",
            PotionKind::Skill => "SkillPotion",
            PotionKind::Power => "PowerPotion",
            PotionKind::Colorless => "ColorlessPotion",
            PotionKind::Cultist => "CultistPotion",
            PotionKind::FruitJuice => "Fruit Juice",
            PotionKind::SneckoOil => "SneckoOil",
            PotionKind::Fairy => "FairyPotion",
            PotionKind::SmokeBomb => "SmokeBomb",
            PotionKind::EntropicBrew => "EntropicBrew",
            PotionKind::LiquidBronze => "LiquidBronze",
            PotionKind::EssenceOfSteel => "EssenceOfSteel",
            PotionKind::GamblersBrew => "GamblersBrew",
            PotionKind::Duplication => "DuplicationPotion",
            PotionKind::LiquidMemories => "LiquidMemories",
            PotionKind::DistilledChaos => "DistilledChaos",
            PotionKind::Cunning => "CunningPotion",
            PotionKind::EssenceOfDarkness => "EssenceOfDarkness",
            PotionKind::Focus => "FocusPotion",
            PotionKind::Capacity => "PotionOfCapacity",
            PotionKind::GhostInAJar => "GhostInAJar",
            PotionKind::HeartOfIron => "HeartOfIron",
            PotionKind::BottledMiracle => "BottledMiracle",
            PotionKind::Stance => "StancePotion",
            PotionKind::Ambrosia => "Ambrosia",
            PotionKind::Poison => "Poison Potion",
            PotionKind::Elixir => "ElixirPotion",
        }
    }

    pub fn from_pool_id(id: &str) -> Option<Self> {
        ALL_POTIONS.iter().copied().find(|k| k.pool_id() == id)
    }

    /// 构造器 rarity（掉落拒绝采样的目标分布）
    pub fn rarity(&self) -> PotionRarity {
        use PotionKind as P;
        match self {
            P::Cultist
            | P::Fairy
            | P::FruitJuice
            | P::GhostInAJar
            | P::HeartOfIron
            | P::SneckoOil
            | P::SmokeBomb
            | P::EntropicBrew
            | P::Ambrosia
            | P::EssenceOfDarkness => PotionRarity::Rare,
            P::Ancient
            | P::Cunning
            | P::DistilledChaos
            | P::Duplication
            | P::Elixir
            | P::EssenceOfSteel
            | P::GamblersBrew
            | P::LiquidBronze
            | P::LiquidMemories
            | P::Capacity
            | P::Regen
            | P::Stance => PotionRarity::Uncommon,
            _ => PotionRarity::Common,
        }
    }

    /// getPotency(ascension)：全表常量（EntropicBrew = 当前药水栏位数，获取时确定）
    pub fn base_potency(&self) -> i32 {
        use PotionKind as P;
        match self {
            P::Fire => 20,
            P::Block => 12,
            P::Swift => 3,
            P::Energy => 2,
            P::Explosive => 10,
            P::Strength => 2,
            P::Weaken => 3,
            P::Fear => 3,
            P::Regen => 5,
            P::Ancient => 1,
            P::Blood => 20, // 百分比
            P::Dexterity => 2,
            P::Steroid => 5,
            P::Speed => 5,
            P::BlessingOfTheForge => 0,
            P::Attack | P::Skill | P::Power | P::Colorless => 1, // 加入手牌的份数
            P::Cultist => 1,
            P::FruitJuice => 5,
            P::SneckoOil => 5, // 抽牌数
            P::Fairy => 30,    // 百分比
            P::SmokeBomb => 0,
            P::EntropicBrew => 0, // 特例：= potionSlots（use 时读）
            P::LiquidBronze => 3,
            P::EssenceOfSteel => 4,
            P::GamblersBrew => 0,
            P::Duplication => 1,
            P::LiquidMemories => 1,
            P::DistilledChaos => 3,
            P::Cunning => 3,
            P::EssenceOfDarkness => 1,
            P::Focus => 2,
            P::Capacity => 2,
            P::GhostInAJar => 1,
            P::HeartOfIron => 6,
            P::BottledMiracle => 2,
            P::Stance => 0,
            P::Ambrosia => 2,
            P::Poison => 6,
            P::Elixir => 0,
        }
    }

    /// 投掷型（isThrown）／需要目标（targetRequired）
    pub fn is_thrown(&self) -> bool {
        matches!(
            self,
            PotionKind::Fire
                | PotionKind::Explosive
                | PotionKind::Weaken
                | PotionKind::Fear
                | PotionKind::Poison
                | PotionKind::SmokeBomb
        )
    }

    pub fn target_required(&self) -> bool {
        matches!(
            self,
            PotionKind::Fire | PotionKind::Weaken | PotionKind::Fear | PotionKind::Poison
        )
    }

    /// canUse 覆写：瓶中精灵被动不可主动使用；
    /// 血药在 turnHasEnded 后不可用（战斗内，WeMeetAgain 事件属 M3）
    pub fn can_use(&self, turn_has_ended: bool) -> bool {
        match self {
            PotionKind::Fairy => false,
            PotionKind::Blood => !turn_has_ended,
            _ => true,
        }
    }
}

/// 药水实例（potency 在获取时定格；EntropicBrew 恒 0——语义上即"填满全部空栏"）
#[derive(Clone, Debug, PartialEq)]
pub struct PotionInst {
    pub kind: PotionKind,
    pub potency: i32,
}

impl PotionInst {
    pub fn new(kind: PotionKind) -> Self {
        Self {
            kind,
            potency: kind.base_potency(),
        }
    }
}

/// 角色掉落池（PotionHelper.getPotions：角色专属 3 + 公共表，按源码顺序）
pub fn pool_for_class(class: &str) -> Vec<PotionKind> {
    let mut v = Vec::new();
    let specials: &[PotionKind] = match class {
        "Ironclad" => &[
            PotionKind::Blood,
            PotionKind::Elixir,
            PotionKind::HeartOfIron,
        ],
        "TheSilent" | "Silent" => &[
            PotionKind::Poison,
            PotionKind::Cunning,
            PotionKind::GhostInAJar,
        ],
        "Defect" => &[
            PotionKind::Focus,
            PotionKind::Capacity,
            PotionKind::EssenceOfDarkness,
        ],
        "Watcher" => &[
            PotionKind::BottledMiracle,
            PotionKind::Stance,
            PotionKind::Ambrosia,
        ],
        // getAll / 无角色：全部角色专属
        _ => &[
            PotionKind::Blood,
            PotionKind::Elixir,
            PotionKind::HeartOfIron,
            PotionKind::Poison,
            PotionKind::Cunning,
            PotionKind::GhostInAJar,
            PotionKind::Focus,
            PotionKind::Capacity,
            PotionKind::EssenceOfDarkness,
            PotionKind::BottledMiracle,
            PotionKind::Stance,
            PotionKind::Ambrosia,
        ],
    };
    v.extend_from_slice(specials);
    for id in [
        "Block Potion",
        "Dexterity Potion",
        "Energy Potion",
        "Explosive Potion",
        "Fire Potion",
        "Strength Potion",
        "Swift Potion",
        "Weak Potion",
        "FearPotion",
        "AttackPotion",
        "SkillPotion",
        "PowerPotion",
        "ColorlessPotion",
        "SteroidPotion",
        "SpeedPotion",
        "BlessingOfTheForge",
        "Regen Potion",
        "Ancient Potion",
        "LiquidBronze",
        "GamblersBrew",
        "EssenceOfSteel",
        "DuplicationPotion",
        "DistilledChaos",
        "LiquidMemories",
        "CultistPotion",
        "Fruit Juice",
        "SneckoOil",
        "FairyPotion",
        "SmokeBomb",
        "EntropicBrew",
    ] {
        v.push(PotionKind::from_pool_id(id).expect("池表 id 笔误"));
    }
    v
}

/// AbstractDungeon.returnRandomPotion(limited) 逐行复刻：
/// 1. roll = potionRng.random(0,99)：<65 COMMON / <90 UNCOMMON / 其余 RARE
/// 2. 拒绝采样：temp = pool[potionRng.random(size-1)]，直到稀有度匹配；
///    limited=true（EntropicBrew 路径）额外跳过 Fruit Juice，且至少重采样一次
pub fn return_random_potion(pool: &[PotionKind], limited: bool, rng: &mut StsRandom) -> PotionKind {
    let roll = rng.random_start_end(0, 99);
    let rarity = if roll < 65 {
        PotionRarity::Common
    } else if roll < 65 + 25 {
        PotionRarity::Uncommon
    } else {
        PotionRarity::Rare
    };
    let mut temp = pool[(rng.random_range(pool.len() as i32 - 1)).max(0) as usize];
    let mut spam_check = limited;
    while temp.rarity() != rarity || spam_check {
        spam_check = limited;
        temp = pool[(rng.random_range(pool.len() as i32 - 1)).max(0) as usize];
        if temp == PotionKind::FruitJuice {
            continue;
        }
        spam_check = false;
    }
    temp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 池表：铁甲 = 3 专属 + 30 公共 = 33 只（PotionHelper.getPotions 逐行）
    #[test]
    fn pool_sizes() {
        let ic = pool_for_class("Ironclad");
        assert_eq!(ic.len(), 33);
        assert!(ic.contains(&PotionKind::Blood));
        assert!(!ic.contains(&PotionKind::Poison)); // 沉默专属不在铁甲池
        let all = pool_for_class("");
        assert_eq!(all.len(), 42); // 12 专属 + 30 公共
    }

    /// 全部 43 只药水元数据自洽（pool_id 可反查）
    #[test]
    fn all_potions_roundtrip() {
        assert_eq!(ALL_POTIONS.len(), 42);
        for k in ALL_POTIONS {
            assert_eq!(PotionKind::from_pool_id(k.pool_id()), Some(*k));
        }
    }

    /// 稀有度分布：33 只铁甲池 = 17C + 9U + 7R
    #[test]
    fn rarity_distribution() {
        let ic = pool_for_class("Ironclad");
        let c = ic
            .iter()
            .filter(|k| k.rarity() == PotionRarity::Common)
            .count();
        let u = ic
            .iter()
            .filter(|k| k.rarity() == PotionRarity::Uncommon)
            .count();
        let r = ic
            .iter()
            .filter(|k| k.rarity() == PotionRarity::Rare)
            .count();
        assert_eq!((c, u, r), (17, 9, 7));
    }

    /// 掉落采样确定性 + 稀有度收敛
    #[test]
    fn random_potion_deterministic() {
        let pool = pool_for_class("Ironclad");
        let mut a = crate::rng::StsRandom::new(42);
        let mut b = crate::rng::StsRandom::new(42);
        for _ in 0..50 {
            assert_eq!(
                return_random_potion(&pool, false, &mut a),
                return_random_potion(&pool, false, &mut b)
            );
        }
        // 大样本：三种稀有度都出现
        let mut seen = std::collections::HashSet::new();
        let mut rng = crate::rng::StsRandom::new(7);
        for _ in 0..500 {
            seen.insert(return_random_potion(&pool, false, &mut rng).rarity());
        }
        assert_eq!(seen.len(), 3);
    }
}
