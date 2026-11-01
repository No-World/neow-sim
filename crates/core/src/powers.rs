//! power（增益/减益）—— 逐位复刻反编译源码（powers/VulnerablePower.java 等）。
//!
//! 语义来源（docs/engine-semantics.md §7 + 源码直读）：
//! - 易伤 Vulnerable：受击方 `atDamageReceive ×1.5`（仅 NORMAL）
//! - 虚弱 Weakened：造成方 `atDamageGive ×0.75`（仅 NORMAL），priority=99
//! - 力量 Strength：造成方 `atDamageGive +amount`（仅 NORMAL），±999 上限，可负
//! - 脆弱 Frail：受击方格挡 `modifyBlock ×0.75`，priority=10
//! - 回合衰减（atEndOfRound）：易伤/虚弱/脆弱 isTurnBased，每轮 -1；
//!   怪物施加的（justApplied）跳过第一次衰减（易伤额外要求 turnHasEnded）

/// DamageInfo.DamageType：NORMAL 走乘区，THORNS/HP_LOSS 不走
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageType {
    Normal,
    Thorns,
    HpLoss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerKind {
    Strength,
    Vulnerable,
    Weak,
    Frail,
    /// 仪式（RitualPower）：每轮末投予力量；怪物侧首轮 skipFirst
    Ritual,
    /// 活动肌肉的回合末力量失效（LoseStrengthPower：EOT 力量 -amount 后移除自身）
    LoseStrength,
    /// 敏捷（DexterityPower：格挡 +flat，±999，可负）
    Dexterity,
    /// 中毒（PoisonPower：怪 EOT 失 HP=层数然后 -1，封顶 9999，绕过格挡）
    Poison,
    /// 金属化（MetallicizePower：玩家 EOT 前 GainBlock(amount)）
    Metallicize,
    /// 再生（Regeneration：EOT 治疗 amount 后 -1 层）
    Regen,
    // —— 以下为本批新增（钩子行为在 combat 中实现） ——
    /// 恶魔形态（Demon Form：回合开始抽牌后 +STR）
    DemonForm,
    /// 狂暴（Berserk：回合开始 +energy）
    Berserk,
    /// 暴虐（Brutality：回合开始抽牌后抽 n 失 n 血）
    Brutality,
    /// 壁垒（Barricade：回合开始不掉格挡）
    Barricade,
    /// 荆棘（Thorns：受击反弹 amount，THORNS 类型）
    Thorns,
    /// 神器（Artifact：抵消下一个施加到自身的减益）
    Artifact,
    /// 无形（IntangiblePlayer：FinalReceive 管线伤害 >1 → 1，轮末衰减）
    IntangiblePlayer,
    /// 能量化（Energized：下回合能量重置时 +amount 后移除）
    Energized,
    /// 燃烧（Combust：EOT 失 1 HP 对所有敌人造成 amount）
    Combust,
    /// 暗之拥（Dark Embrace：每消耗一张牌抽 amount）
    DarkEmbrace,
    /// 怒火（Rage：本回合每打出一张攻击牌获得 amount 格挡，EOT 移除）
    Rage,
    /// 蜷缩（Curl Up：首次受到非致死 NORMAL 伤害时获得 amount 格挡后移除）
    CurlUp,
    /// 孢子云（Spore Cloud：死亡时对玩家施加 amount 层易伤）
    SporeCloud,
    /// 专注（Focus：充能球被动/引出 +amount，Plasma 不受影响）
    Focus,
    // —— 药水系统新增（钩子行为在 combat 中实现） ——
    /// 多层护甲（Plated Armor：EOT 获得 amount 格挡；受到有实际掉血的
    /// NORMAL 伤害时 -1 层，归零移除；wasHPLost 钩子，PlatedArmorPower.java）
    PlatedArmor,
    /// 复制（DuplicationPower：下一张打出的牌额外免费结算一次，每次 -1，
    /// 归零移除；跨回合持续，非 turn-based）
    Duplication,
    /// 敏捷失效（DexLoss：EOT 敏捷 -amount 后移除自身，LoseDexterityPower.java）
    LoseDexterity,
    // —— Act 2 怪物系（钩子在 combat 中实现） ——
    /// 飞行（Flight：受方 atDamageFinalReceive NORMAL 伤害减半；
    /// 每次非致死 NORMAL 受击 -1 层，归零移除 → Byrd GROUNDED 换态）
    Flight,
    /// 可延展（Malleable：每次非致死 NORMAL 受击获得 amount 格挡，
    /// 且 amount+1 递增；无衰减）
    Malleable,
    /// 咒缚（Hex：玩家每打出一张非攻击牌，往抽牌堆随机位置塞
    /// amount 张晕眩，HexPower.onUseCard）
    Hex,
    /// 混乱（Confusion：每次抽牌时费用在 0..3 重掣（永改，
    /// cardRandomRng.random(3)，仅 cost>=0，ConfusionPower.onCardDraw）
    Confusion,
    /// 愤怒（Angry：受方每次非 THORNS/HP_LOSS 伤害 → 力量 +amount，
    /// AngryPower.onAttacked；战鬼战前 1 层）
    Angry,
    /// 静滞（Stasis：铜球吸走的一张玩家卡的标记；死亡时归还手牌）
    Stasis,
    // —— Act 3 怪物系 ——
    /// 消逝（Fading：自身回合开始层数-1，归零时自灭；Transient）
    Fading,
    /// 变形（Shifting：受方每次受击 力量 -伤害值，同时累积回复力量；
    /// Transient 越打越弱，回合末力量回归）
    Shifting,
    /// 力量回复（GainStrength：轮末 力量 +amount 后移除）
    GainStrength,
    /// 紧缠（Constricted：玩家回合末受到 amount 点 THORNS 直伤；SpireGrowth）
    Constricted,
    /// 反应（Reactive：受方每次非致死受击 → 重掷招式（消耗 aiRng）；WrithingMass）
    Reactive,
    // —— Act 3 精英/Boss ——
    /// 迟缓（Slow：受方受到的 NORMAL 伤害 ×(1+amount×10%)；
    /// 玩家每打出一张牌 amount+1，轮末归零；GiantHead）
    Slow,
    /// 时滞（TimeWarp：玩家每打出一张牌计数+1，满 12 立即终止玩家回合
    /// 并清零；TimeEater）
    TimeWarp,
    /// 好奇（Curiosity：玩家每打出一张能力牌 → 自身力量 +amount；AwakenedOne）
    Curiosity,
    /// 再生怪（RegenerateMonster：自身回合开始回复 amount；AwakenedOne）
    RegenerateMonster,
    // —— Act 4（终局）——
    /// 无敌（Invincible：单次受击上限=剩余额度，额度随受击递减；
    /// 心脏 300/200 = 整场战斗最多吃这么多伤害）
    Invincible,
    /// 亡律（BeatOfDeath：玩家每打出一张牌 → 受 amount 点 THORNS 直伤）
    BeatOfDeath,
    /// 痛击（PainfulStabs：玩家每次对持有者造成非 THORNS 伤害 → 失 2 HP）
    PainfulStabs,
    // —— 遗物系统新增（钩子在 combat/relics 中实现） ——
    /// 干劲（VigorPower：下一张攻击牌 NORMAL 伤害 +amount，打出攻击牌后移除；
    /// watcher/VigorPower.java，Akabeko 战斗开始授予 8 层）
    Vigor,
    /// 笔尖（PenNibPower：下一张攻击牌 NORMAL 伤害 ×2，打出攻击牌后移除；
    /// powers/PenNibPower.java，Pen Nib 遗物第 9 张攻击后授予；
    /// priority=6：晚于力量/干劲（5），早于虚弱（99））
    PenNib,
    // —— 角色补充批次 ——
    /// 禁抽（NoDrawPower：本回合不能再抽牌，轮末移除；Impervious 等）
    NoDraw,
    /// 真言（MantraPower：屈身/祈祷等叠加；满 10 → 自动进入神性并清零）
    Mantra,
    // —— power 补缺第一批（每只解锁 1 张卡；钩子在 combat 接线）——
    /// 剧毒瘴气（NoxiousFumesPower：抽牌后回合开始 → 全体敌人中毒 amount）
    NoxiousFumes,
    /// 无限之刃（InfiniteBladesPower：回合开始 → Shiv×amount 入手）
    InfiniteBlades,
    /// 心之堡垒（MentalFortressPower：姿态切换 → 获 amount 格挡）
    MentalFortress,
    /// 如水（LikeWaterPower：平静中回合结束 → 获 amount 格挡）
    LikeWater,
    /// 奉献（DevotionPower：抽牌后回合开始 → 真言 +amount）
    Devotion,
    /// 风暴（StormPower：打出能力牌 → 充能 amount 颗闪电球）
    Storm,
    /// 无痛（FeelNoPainPower：消耗一张牌 → 获 amount 格挡）
    FeelNoPain,
    /// 破裂（RupturePower：因自身卡牌失 HP → 力量 +amount）
    Rupture,
    /// 余像（AfterImagePower：打出任意牌 → 获 amount 格挡）
    AfterImage,
    /// 自我修复（RepairPower：战斗胜利 → 回 amount 血）
    Repair,
    /// 下回合抽牌（DrawCardNextTurnPower：下回合抽牌后多抽 amount，一次性）
    DrawCardNextTurn,
    /// 下回合格挡（NextTurnBlockPower：下回合开始获 amount 格挡，一次性）
    NextTurnBlock,
    /// 抽牌量提升（DrawPower：gameHandSize +amount，持续）
    DrawBonus,
    /// 炸弹（TheBombPower：amount 编码 dmg×10+倒数；EOT 倒数-1，归零全体 THORNS 伤）
    TheBomb,
    /// 下回合愤怒（WrathNextTurnPower：下回合开始进愤怒，一次性）
    WrathNextTurn,
    // ── power 补缺第二批（25 只）──
    Accuracy,
    Blur,
    Buffer,
    Burst,
    DoubleTap,
    EchoForm,
    Envenom,
    Evolve,
    FireBreathing,
    FlameBarrier,
    Juggernaut,
    LockOn,
    Magnetism,
    Mayhem,
    Nirvana,
    NoBlock,
    Omega,
    Phantasmal,
    Rushdown,
    Sadistic,
    StaticDischarge,
    ThousandCuts,
    WaveOfTheHand,
    WraithForm,
}

impl PowerKind {
    /// power 的 type==DEBUFF 判定（Artifact 拦截依据：ApplyPowerAction:125）
    pub fn is_debuff(&self) -> bool {
        matches!(
            self,
            PowerKind::Vulnerable
                | PowerKind::Weak
                | PowerKind::Frail
                | PowerKind::Poison
                | PowerKind::Hex
                | PowerKind::Confusion
                | PowerKind::Constricted
        )
    }
}

/// atEndOfRound 衰减结果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decay {
    /// justApplied 消耗，本轮不减
    SkipFirst,
    /// 仍在（如 Strength 无衰减 / 层数未归零）
    Stay,
    /// 层数-1 后仍存在
    Reduced(i32),
    /// 移除（归零或本来就是 0）
    Remove,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Power {
    pub kind: PowerKind,
    pub amount: i32,
    pub just_applied: bool,
    /// Paper Crane 裁定：虚弱乘区 60%（战斗级标记，随 Power 传递）
    pub has_paper_crane: bool,
    /// Paper Frog 裁定：易伤乘区 175%
    pub has_paper_frog: bool,
}

impl Power {
    pub fn new(
        kind: PowerKind,
        amount: i32,
        source_is_monster: bool,
        turn_has_ended: bool,
    ) -> Self {
        let just_applied = match kind {
            // VulnerablePower: if (turnHasEnded && isSourceMonster)
            PowerKind::Vulnerable => source_is_monster && turn_has_ended,
            // WeakPower / FrailPower: if (isSourceMonster)
            PowerKind::Weak | PowerKind::Frail => source_is_monster,
            // RitualPower: skipFirst = true（怪物侧首轮不投予）；其余无 skip
            PowerKind::Ritual => true,
            // IntangiblePower：justApplied=true —— 施加后跳过第一次轮末衰减
            // （Nemesis 回合尾自动无形能撑过整个玩家回合的关键）
            PowerKind::IntangiblePlayer => true,
            _ => false,
        };
        Self {
            kind,
            amount,
            just_applied,
            has_paper_crane: false,
            has_paper_frog: false,
        }
    }

    pub fn id(&self) -> &'static str {
        match self.kind {
            PowerKind::Strength => "Strength",
            PowerKind::Vulnerable => "Vulnerable",
            PowerKind::Weak => "Weakened",
            PowerKind::Frail => "Frail",
            PowerKind::Ritual => "Ritual",
            PowerKind::LoseStrength => "LoseStrengthPower",
            PowerKind::Dexterity => "Dexterity",
            PowerKind::Poison => "Poison",
            PowerKind::Metallicize => "Metallicize",
            PowerKind::Regen => "Regeneration",
            PowerKind::DemonForm => "Demon Form",
            PowerKind::Berserk => "Berserk",
            PowerKind::Brutality => "Brutality",
            PowerKind::Barricade => "Barricade",
            PowerKind::Thorns => "Thorns",
            PowerKind::Artifact => "Artifact",
            PowerKind::IntangiblePlayer => "IntangiblePlayer",
            PowerKind::Energized => "Energized",
            PowerKind::Combust => "Combust",
            PowerKind::DarkEmbrace => "Dark Embrace",
            PowerKind::Rage => "Rage",
            PowerKind::CurlUp => "Curl Up",
            PowerKind::SporeCloud => "Spore Cloud",
            PowerKind::Focus => "Focus",
            PowerKind::PlatedArmor => "Plated Armor",
            PowerKind::Duplication => "DuplicationPower",
            PowerKind::LoseDexterity => "DexLoss",
            PowerKind::Flight => "Flight",
            PowerKind::Malleable => "Malleable",
            PowerKind::Hex => "Hex",
            PowerKind::Confusion => "Confusion",
            PowerKind::Angry => "Angry",
            PowerKind::Stasis => "Stasis",
            PowerKind::Fading => "Fading",
            PowerKind::Shifting => "Shifting",
            PowerKind::GainStrength => "GainStrength",
            PowerKind::Constricted => "Constricted",
            PowerKind::Reactive => "Reactive",
            PowerKind::Slow => "Slow",
            PowerKind::TimeWarp => "Time Warp",
            PowerKind::Curiosity => "Curiosity",
            PowerKind::RegenerateMonster => "Regenerate",
            PowerKind::Invincible => "Invincible",
            PowerKind::BeatOfDeath => "Beat of Death",
            PowerKind::PainfulStabs => "Painful Stabs",
            PowerKind::Vigor => "Vigor",
            PowerKind::NoDraw => "NoDrawPower",
            PowerKind::Mantra => "MantraPower",
            PowerKind::NoxiousFumes => "Noxious Fumes",
            PowerKind::InfiniteBlades => "Infinite Blades",
            PowerKind::MentalFortress => "MentalFortress",
            PowerKind::LikeWater => "LikeWater",
            PowerKind::Devotion => "Devotion",
            PowerKind::Storm => "Storm",
            PowerKind::FeelNoPain => "Feel No Pain",
            PowerKind::Rupture => "Rupture",
            PowerKind::AfterImage => "After Image",
            PowerKind::Repair => "Repair",
            PowerKind::DrawCardNextTurn => "DrawCardNextTurnPower",
            PowerKind::NextTurnBlock => "NextTurnBlockPower",
            PowerKind::DrawBonus => "DrawPower",
            PowerKind::TheBomb => "The Bomb",
            PowerKind::WrathNextTurn => "WrathNextTurnPower",
            PowerKind::PenNib => "Pen Nib",
            PowerKind::Accuracy => "AccuracyPower",
            PowerKind::Blur => "BlurPower",
            PowerKind::Buffer => "Buffer",
            PowerKind::Burst => "BurstPower",
            PowerKind::DoubleTap => "DoubleTapPower",
            PowerKind::EchoForm => "EchoPower",
            PowerKind::Envenom => "EnvenomPower",
            PowerKind::Evolve => "EvolvePower",
            PowerKind::FireBreathing => "FireBreathingPower",
            PowerKind::FlameBarrier => "FlameBarrierPower",
            PowerKind::Juggernaut => "JuggernautPower",
            PowerKind::LockOn => "LockOnPower",
            PowerKind::Magnetism => "MagnetismPower",
            PowerKind::Mayhem => "MayhemPower",
            PowerKind::Nirvana => "NirvanaPower",
            PowerKind::NoBlock => "NoBlockPower",
            PowerKind::Omega => "OmegaPower",
            PowerKind::Phantasmal => "PhantasmalPower",
            PowerKind::Rushdown => "RushdownPower",
            PowerKind::Sadistic => "SadisticPower",
            PowerKind::StaticDischarge => "StaticDischargePower",
            PowerKind::ThousandCuts => "ThousandCutsPower",
            PowerKind::WaveOfTheHand => "WaveOfTheHandPower",
            PowerKind::WraithForm => "WraithFormPower",
        }
    }

    /// strength 叠加 ±999 上限、归零移除；Plated Armor 叠加封顶 999（stackPower）
    pub fn stack_cap(&self) -> Option<i32> {
        match self.kind {
            PowerKind::Strength => Some(999),
            PowerKind::PlatedArmor => Some(999),
            _ => None,
        }
    }

    /// 是否走通用轮末衰减（易伤/虚弱/脆弱/无形）；毒/再生有自己的 tick，其余不衰减
    pub fn is_turn_based(&self) -> bool {
        matches!(
            self.kind,
            PowerKind::Vulnerable
                | PowerKind::Weak
                | PowerKind::Frail
                | PowerKind::IntangiblePlayer
                | PowerKind::NoDraw
        )
    }

    /// atDamageGive：造成伤害方的 power（StrengthPower: +flat；WeakPower: ×0.75）
    pub fn at_damage_give(&self, dmg: f32, ty: DamageType) -> f32 {
        match (self.kind, ty) {
            (PowerKind::Strength, DamageType::Normal) => dmg + self.amount as f32,
            (PowerKind::Weak, DamageType::Normal) => {
                // Paper Crane：虚弱效果 60%（替代默认 75%）
                if self.has_paper_crane {
                    dmg * 0.6
                } else {
                    dmg * 0.75
                }
            }
            (PowerKind::Vigor, DamageType::Normal) => dmg + self.amount as f32,
            (PowerKind::PenNib, DamageType::Normal) => dmg * 2.0,
            _ => dmg,
        }
    }

    /// atDamageReceive：受击方的 power（VulnerablePower: ×1.5）
    pub fn at_damage_receive(&self, dmg: f32, ty: DamageType) -> f32 {
        match (self.kind, ty) {
            (PowerKind::Vulnerable, DamageType::Normal) => {
                // Paper Frog：易伤效果 175%（替代默认 150%）
                if self.has_paper_frog {
                    dmg * 1.75
                } else {
                    dmg * 1.5
                }
            }
            (PowerKind::Slow, DamageType::Normal) => dmg * (1.0 + self.amount as f32 * 0.1),
            _ => dmg,
        }
    }

    /// atDamageFinalGive/FinalReceive：IntangiblePlayer 在此环节把伤害 >1 截为 1（源码无类型过滤）；
    /// Flight（Byrd）在受方环节把 NORMAL 伤害减半（calculateDamageTakenAmount）
    pub fn at_damage_final_give(&self, dmg: f32, _ty: DamageType) -> f32 {
        dmg
    }

    pub fn at_damage_final_receive(&self, dmg: f32, ty: DamageType) -> f32 {
        match self.kind {
            PowerKind::IntangiblePlayer if dmg > 1.0 => 1.0,
            PowerKind::Flight if ty == DamageType::Normal => dmg / 2.0,
            _ => dmg,
        }
    }

    /// modifyBlock（FrailPower: ×0.75；在受格挡方的 power 列表上迭代）
    pub fn modify_block(&self, block: f32) -> f32 {
        match self.kind {
            PowerKind::Frail => block * 0.75,
            PowerKind::Dexterity => block + self.amount as f32,
            _ => block,
        }
    }

    /// atEndOfRound：按源码 `justApplied → amount==0 ? Remove : Reduce(1)`，
    /// ReducePowerAction 归零即移除。Strength 无衰减。
    pub fn at_end_of_round(&mut self) -> Decay {
        if !self.is_turn_based() {
            return Decay::Stay;
        }
        if self.just_applied {
            self.just_applied = false;
            return Decay::SkipFirst;
        }
        if self.amount == 0 {
            return Decay::Remove;
        }
        self.amount -= 1;
        if self.amount == 0 {
            Decay::Remove
        } else {
            Decay::Reduced(self.amount)
        }
    }
}

/// ApplyPowerAction + stackPower 语义：同类叠加；Strength ±999 上限且归零移除
pub fn apply_power(
    target: &mut Vec<Power>,
    kind: PowerKind,
    amount: i32,
    source_is_monster: bool,
    turn_has_ended: bool,
) {
    if let Some(p) = target.iter_mut().find(|p| p.kind == kind) {
        p.amount += amount;
        if let Some(cap) = p.stack_cap() {
            p.amount = p.amount.clamp(-cap, cap);
        }
        if p.amount == 0 {
            target.retain(|q| q.kind != kind);
        }
    } else {
        let mut p = Power::new(kind, amount, source_is_monster, turn_has_ended);
        if let Some(cap) = p.stack_cap() {
            p.amount = p.amount.clamp(-cap, cap);
        }
        target.push(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与游戏数值常识对拍：floor(6×1.5)=9、floor(11×0.75)=8、floor(5×0.75)=3
    #[test]
    fn multipliers_exact() {
        let vuln = Power::new(PowerKind::Vulnerable, 2, false, false);
        assert_eq!(vuln.at_damage_receive(6.0, DamageType::Normal), 9.0);
        // THORNS/HP_LOSS 不吃乘区
        assert_eq!(vuln.at_damage_receive(6.0, DamageType::Thorns), 6.0);

        let weak = Power::new(PowerKind::Weak, 1, false, false);
        assert_eq!(weak.at_damage_give(11.0, DamageType::Normal), 8.25);

        let frail = Power::new(PowerKind::Frail, 1, false, false);
        assert_eq!(frail.modify_block(5.0), 3.75);

        let str_ = Power::new(PowerKind::Strength, 3, false, false);
        assert_eq!(str_.at_damage_give(11.0, DamageType::Normal), 14.0);
        assert_eq!(str_.at_damage_give(11.0, DamageType::HpLoss), 11.0);
    }

    /// Bash 式衰减：施加 2 层 → 第一轮末 1 → 第二轮末移除
    #[test]
    fn vulnerable_decay_player_source() {
        let mut p = Power::new(PowerKind::Vulnerable, 2, false, false);
        assert_eq!(p.at_end_of_round(), Decay::Reduced(1));
        assert_eq!(p.at_end_of_round(), Decay::Remove);
    }

    /// 怪物施加的虚弱：跳过第一次衰减（WeakPower.justApplied）
    #[test]
    fn weak_decay_monster_source() {
        let mut p = Power::new(PowerKind::Weak, 1, true, true);
        assert_eq!(p.at_end_of_round(), Decay::SkipFirst);
        assert_eq!(p.at_end_of_round(), Decay::Remove);
    }

    /// Strength 叠加 ±999 上限、归零移除（stackPower 源码）
    #[test]
    fn strength_stack_semantics() {
        let mut v = vec![];
        apply_power(&mut v, PowerKind::Strength, 3, false, false);
        apply_power(&mut v, PowerKind::Strength, -3, false, false);
        assert!(v.is_empty());
        apply_power(&mut v, PowerKind::Strength, 1000, false, false);
        assert_eq!(v[0].amount, 999);
    }
}
