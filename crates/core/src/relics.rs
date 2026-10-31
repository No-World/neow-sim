//! 遗物系统（第一批：战斗内触发遗物）—— 逐只对照反编译源码 `relics/*.java`。
//!
//! 范围：atBattleStart / atTurnStart / onUseCard / onExhaust / onAttacked /
//! onLoseHpLast / wasHPLost / onShuffle / onPlayerEndTurn / onMonsterDeath /
//! onVictory 有实质战斗行为的遗物；商店/事件/宝箱获取与局外钩子（max HP、
//! energy master、masterHandSize 等）留给 M3 局外层。
//!
//! 钩子接入点（combat.rs，对照 AbstractRoom/GameActionManager 源码时序）：
//! - 战斗开始：`new_with` 末尾 `relics_battle_start()`（atPreBattle → atBattleStart
//!   → 首轮 atTurnStart；atBattleStart 动作在真游戏中 addToBot 于初始抽 5 之后
//!   结算，数值上与同步结算等价——MarkOfPain 的随机插入、蛇戒/备包的补抽同理）
//! - 出牌：`play_card` / `play_card_free` 的 `relics_on_use_card`（效果追加到该牌
//!   的动作队列尾部，即卡牌自身效果之后结算）
//! - 回合开始：`end_turn` 下回合序列中 loseBlock 之后、抽 5 之前
//!   `relics_at_turn_start`（真游戏中 atTurnStart 在 loseBlock 前同步调用、
//!   动作在 loseBlock 后入队执行，格挡类遗物净效果一致）
//! - 回合结束：`end_turn` 开头 `relics_on_player_end_turn`（先于玩家 powers EOT，
//!   对照 callEndOfTurnActions：applyEndOfTurnRelics 最先）
//! - 受击/失血：`player_deal_damage`（Torii.onAttacked → TungstenRod.onLoseHpLast
//!   → CentennialPuzzle.wasHPLost，对照 AbstractPlayer.damage:1410-1429 的顺序）
//! - 洗牌：`Q::ShuffleDraw` 与抽牌堆耗尽重洗（EmptyDeckShuffleAction/ShuffleAction
//!   都触发 onShuffle；战斗开始的 initializeDeck 洗牌不触发）
//! - 消耗：全部入消耗堆的路径（CardGroup.moveToExhaustPile：遗物先于 power）

use crate::cards::CardInst;
use crate::combat::Combat;
use crate::powers::{apply_power, DamageType, PowerKind};
use crate::relics::RelicKind::*;
use crate::state::Stance;

/// 战斗内有行为的第一批遗物（数值全部逐位对照源码构造器/钩子方法）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelicKind {
    // ── 战斗开始类（atPreBattle / atBattleStart / 首回合） ──
    /// Vajra.java：战斗开始 +1 力量（ApplyPowerAction Strength 1）
    Vajra,
    /// BronzeScales.java：战斗开始 +3 荆棘（ThornsPower 3）
    BronzeScales,
    /// BloodVial.java：战斗开始回复 2 HP（HealAction 2）
    BloodVial,
    /// BagOfPreparation.java：战斗开始额外抽 2 张（初始抽 5 之后 → 手牌 7）
    BagOfPreparation,
    /// Anchor.java：战斗开始获得 10 格挡（GainBlockAction，无敏捷/脆弱修正）
    Anchor,
    /// BagOfMarbles.java：战斗开始所有敌人 1 层易伤
    BagOfMarbles,
    /// RedMask.java：战斗开始所有敌人 1 层虚弱
    RedMask,
    /// PhilosopherStone.java：战斗开始所有敌人 +1 力量（m.addPower 直加，
    /// 不走 ApplyPowerAction）
    PhilosopherStone,
    /// OddlySmoothStone.java：战斗开始 +1 敏捷
    OddlySmoothStone,
    /// TwistedFunnel.java：战斗开始所有存活敌人 4 层中毒
    TwistedFunnel,
    /// ClockworkSouvenir.java：战斗开始获得 1 层神器（Artifact 1）
    ClockworkSouvenir,
    /// DataDisk.java：战斗开始 +1 专注
    DataDisk,
    /// CrackedCore.java：atPreBattle 充能 1 闪电球（Defect 初始遗物）
    CrackedCore,
    /// SymbioticVirus.java：atPreBattle 充能 1 暗球
    SymbioticVirus,
    /// NuclearBattery.java：atPreBattle 充能 1 等离子球
    NuclearBattery,
    /// SnakeRing.java（ID "Ring of the Snake"）：战斗开始额外抽 2 张（→ 手牌 7）
    SnakeRing,
    /// TeardropLocket.java：战斗开始进入平静姿态（ChangeStanceAction "Calm"）
    TeardropLocket,
    /// Lantern.java：atTurnStart 首回合 +1 能量（firstTurn 内部旗标，
    /// 由 turn==1 等价判定）
    Lantern,
    /// MarkOfPain.java：战斗开始往抽牌堆随机位置塞 2 张创伤
    /// （MakeTempCardInDrawPileAction randomSpot，初始抽 5 之后，cardRandomRng）
    MarkOfPain,
    /// DuVuDoll.java：战斗开始获得等同 counter 的力量（counter = 主牌组诅咒数，
    /// 局外层 onMasterDeckChange 维护）
    DuVuDoll,
    /// Girya.java：战斗开始获得等同 counter 的力量（counter = 篝火举重次数 0-3，
    /// 局外层维护）
    Girya,
    /// GremlinHorn.java：onMonsterDeath——非最后一只怪死亡时 +1 能量并抽 1 张
    GremlinHorn,
    /// BurningBlood.java：onVictory 回复 6 HP（战斗内无行为；见 `Combat::on_victory`）
    BurningBlood,
    /// BlackBlood.java：onVictory 回复 12 HP（燃血升级版，战斗内同样无行为）
    BlackBlood,
    /// FaceOfCleric.java：onVictory 最大生命 +1（局外永久；`on_victory` 返回值）
    FaceOfCleric,
    // ── 出牌计数类（onUseCard） ──
    /// Akabeko.java：战斗开始获得 8 层干劲（VigorPower：下一张攻击 +8 伤害）
    Akabeko,
    /// PenNib.java：每打出一张攻击 counter+1；第 9 张后获得笔尖能力
    /// （PenNibPower：下一张攻击 NORMAL 伤害 ×2）；第 10 张归零。
    /// counter 跨战斗持久（构造器 0；atBattleStart 时 counter==9 亦授能力）
    PenNib,
    /// Shuriken.java：每回合第 3 张攻击 +1 力量（atTurnStart counter=0；
    /// onVictory counter=-1，跨战斗持久）
    Shuriken,
    /// Kunai.java：每回合第 3 张攻击 +1 敏捷（同手里剑计数语义）
    Kunai,
    /// OrnamentalFan.java：每回合第 3 张攻击获得 4 格挡（同手里剑计数语义）
    OrnamentalFan,
    /// Nunchaku.java：每打出 10 张攻击 +1 能量（counter %10，跨战斗持久）
    Nunchaku,
    /// LetterOpener.java：每回合第 3 张技能对所有敌人造成 5 点 THORNS 直伤
    /// （DamageInfo.createDamageMatrix(5, true)，无力量/虚弱/易伤乘区）
    LetterOpener,
    /// BirdFacedUrn.java：每打出一张能力牌回复 2 HP
    BirdFacedUrn,
    /// CharonsAshes.java：每消耗一张牌对所有敌人造成 3 点 THORNS 直伤
    CharonsAshes,
    // ── 回合/洗牌类（atTurnStart / onShuffle / onPlayerEndTurn） ──
    /// Sundial.java：每洗牌 3 次 +2 能量（onShuffle；counter 跨战斗持久，onEquip=0）
    Sundial,
    /// HappyFlower.java：每 3 回合 +1 能量（atTurnStart counter==-1 ? +2 : +1，
    /// ==3 时归零触发；counter 跨战斗持久）
    HappyFlower,
    /// ArtOfWar.java：上回合未打出攻击牌 → 本回合开始 +1 能量
    /// （首回合 firstTurn 不触发；counter 0=本回合未打攻击/1=已打）
    ArtOfWar,
    /// HornCleat.java：每场战斗一次，第 2 回合开始获得 14 格挡
    /// （counter==2 触发后置 -1 = grayscale）
    HornCleat,
    /// CaptainsWheel.java：每场战斗一次，第 3 回合开始获得 18 格挡（同 HornCleat）
    CaptainsWheel,
    /// IncenseBurner.java：每 6 回合开始获得 1 层无形（counter==-1 ? +2 : +1，
    /// ==6 归零触发）
    IncenseBurner,
    /// StoneCalendar.java：第 7 回合结束时对所有敌人造成 52 点 THORNS 直伤
    /// （atBattleStart counter=0，atTurnStart ++，==7 时恰好一次）
    StoneCalendar,
    /// MercuryHourglass.java：每回合开始（含战斗首回合）对所有敌人
    /// 造成 3 点 THORNS 直伤
    MercuryHourglass,
    /// Brimstone.java：每回合开始（含首回合）+2 力量，所有敌人 +1 力量
    Brimstone,
    /// Inserter.java：每 2 回合 +1 充能球槽位（counter==-1 ? +2 : +1，==2 归零）
    Inserter,
    /// RunicCapacitor.java：首回合开始 +3 充能球槽位（firstTurn 一次性）
    RunicCapacitor,
    /// FrozenCore.java：回合结束时若有空球槽充能 1 冰霜球（Defect Boss 遗物）
    FrozenCore,
    /// Orichalcum.java：回合结束时若没有格挡获得 6 格挡
    Orichalcum,
    // ── 受击/失血类（onAttacked / onLoseHpLast / wasHPLost） ──
    /// TungstenRod.java：每次失去 HP 的量 -1（onLoseHpLast，任何伤害类型、
    /// 格挡吸收之后判定）
    TungstenRod,
    /// Torii.java：受到的（格挡后）2..=5 点非 THORNS/HP_LOSS 攻击伤害改为 1
    /// （onAttacked，需 info.owner 非空 = 怪物攻击）
    Torii,
    /// CentennialPuzzle.java：每场战斗第一次实际掉血时抽 3 张
    /// （wasHPLost，counter 0=未用/1=已用，atPreBattle 清零）
    CentennialPuzzle,
}

/// 全部已实现遗物（覆盖率统计 & 遍历用）
pub const ALL_RELICS: &[RelicKind] = &[
    Vajra,
    BronzeScales,
    BloodVial,
    BagOfPreparation,
    Anchor,
    BagOfMarbles,
    RedMask,
    PhilosopherStone,
    OddlySmoothStone,
    TwistedFunnel,
    ClockworkSouvenir,
    DataDisk,
    CrackedCore,
    SymbioticVirus,
    NuclearBattery,
    SnakeRing,
    TeardropLocket,
    Lantern,
    MarkOfPain,
    DuVuDoll,
    Girya,
    GremlinHorn,
    BurningBlood,
    BlackBlood,
    FaceOfCleric,
    Akabeko,
    PenNib,
    Shuriken,
    Kunai,
    OrnamentalFan,
    Nunchaku,
    LetterOpener,
    BirdFacedUrn,
    CharonsAshes,
    Sundial,
    HappyFlower,
    ArtOfWar,
    HornCleat,
    CaptainsWheel,
    IncenseBurner,
    StoneCalendar,
    MercuryHourglass,
    Brimstone,
    Inserter,
    RunicCapacitor,
    FrozenCore,
    Orichalcum,
    TungstenRod,
    Torii,
    CentennialPuzzle,
];

impl RelicKind {
    /// 游戏内 relicId（对应源码类 `public static final String ID`）
    pub fn id(&self) -> &'static str {
        match self {
            Vajra => "Vajra",
            BronzeScales => "Bronze Scales",
            BloodVial => "Blood Vial",
            BagOfPreparation => "Bag of Preparation",
            Anchor => "Anchor",
            BagOfMarbles => "Bag of Marbles",
            RedMask => "Red Mask",
            PhilosopherStone => "Philosopher's Stone",
            OddlySmoothStone => "Oddly Smooth Stone",
            TwistedFunnel => "TwistedFunnel",
            ClockworkSouvenir => "ClockworkSouvenir",
            DataDisk => "DataDisk",
            CrackedCore => "Cracked Core",
            SymbioticVirus => "Symbiotic Virus",
            NuclearBattery => "Nuclear Battery",
            SnakeRing => "Ring of the Snake",
            TeardropLocket => "TeardropLocket",
            Lantern => "Lantern",
            MarkOfPain => "Mark of Pain",
            DuVuDoll => "Du-Vu Doll",
            Girya => "Girya",
            GremlinHorn => "Gremlin Horn",
            BurningBlood => "Burning Blood",
            BlackBlood => "Black Blood",
            FaceOfCleric => "FaceOfCleric",
            Akabeko => "Akabeko",
            PenNib => "Pen Nib",
            Shuriken => "Shuriken",
            Kunai => "Kunai",
            OrnamentalFan => "Ornamental Fan",
            Nunchaku => "Nunchaku",
            LetterOpener => "Letter Opener",
            BirdFacedUrn => "Bird Faced Urn",
            CharonsAshes => "Charon's Ashes",
            Sundial => "Sundial",
            HappyFlower => "Happy Flower",
            ArtOfWar => "Art of War",
            HornCleat => "HornCleat",
            CaptainsWheel => "CaptainsWheel",
            IncenseBurner => "Incense Burner",
            StoneCalendar => "StoneCalendar",
            MercuryHourglass => "Mercury Hourglass",
            Brimstone => "Brimstone",
            Inserter => "Inserter",
            RunicCapacitor => "Runic Capacitor",
            FrozenCore => "FrozenCore",
            Orichalcum => "Orichalcum",
            TungstenRod => "TungstenRod",
            Torii => "Torii",
            CentennialPuzzle => "Centennial Puzzle",
        }
    }
}

/// 一件遗物实例：种类 + 计数器（语义见各变体注释；初始值一律 0 —— 源码
/// 构造器/onEquip 均为 0 或无计数器，跨战斗持久计数由局外层回填）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelicInst {
    pub kind: RelicKind,
    pub counter: i32,
}

impl RelicInst {
    pub fn new(kind: RelicKind) -> Self {
        Self { kind, counter: 0 }
    }
}

impl Combat {
    // ── 查询/改写小工具 ──

    pub fn has_relic(&self, kind: RelicKind) -> bool {
        self.player_relics.iter().any(|r| r.kind == kind)
    }

    fn relic_counter(&self, kind: RelicKind) -> Option<i32> {
        self.player_relics
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| r.counter)
    }

    fn set_relic_counter(&mut self, kind: RelicKind, counter: i32) {
        if let Some(r) = self.player_relics.iter_mut().find(|r| r.kind == kind) {
            r.counter = counter;
        }
    }

    /// 对所有存活敌人造成 THORNS 直伤（DamageAllEnemiesAction + createDamageMatrix：
    /// 无力量/虚弱/易伤乘区，不触发 CurlUp/Angry/Flight 等受击钩子）
    fn relic_thorns_damage_all(&mut self, dmg: i32) {
        for mi in 0..self.monsters.len() {
            if self.alive(mi) {
                self.hit_monster(mi, dmg, DamageType::Thorns);
            }
        }
    }

    // ── 战斗开始（AbstractPlayer.preBattlePrep / AbstractRoom 战斗开始块） ──

    /// atPreBattle → atBattleStart → 首轮 atTurnStart（new_with 末尾调用）。
    /// atBattleStart 动作真游戏里 addToBot 于初始抽 5 之后结算；抽牌/随机插牌
    /// 类遗物在此已按该时序处理（初始 5 张在手后补抽/插入）。
    pub(crate) fn relics_battle_start(&mut self) {
        // atPreBattle：充能球（channelOrb 直调，无动作队列）
        if self.has_relic(CrackedCore) {
            self.exec(crate::combat::Q::Channel {
                kind: crate::state::OrbKind::Lightning,
                auto_evoke: true,
            });
        }
        if self.has_relic(SymbioticVirus) {
            self.exec(crate::combat::Q::Channel {
                kind: crate::state::OrbKind::Dark,
                auto_evoke: true,
            });
        }
        if self.has_relic(NuclearBattery) {
            self.exec(crate::combat::Q::Channel {
                kind: crate::state::OrbKind::Plasma,
                auto_evoke: true,
            });
        }
        // atBattleStart（ApplyPowerAction 走神器拦截路径）
        if self.has_relic(Vajra) {
            self.apply_power_to_player_checked(PowerKind::Strength, 1, false);
        }
        // Akabeko.java：VigorPower 8（下一张攻击 +8）
        if self.has_relic(Akabeko) {
            self.apply_power_to_player_checked(PowerKind::Vigor, 8, false);
        }
        if self.has_relic(BronzeScales) {
            self.apply_power_to_player_checked(PowerKind::Thorns, 3, false);
        }
        if self.has_relic(BloodVial) {
            self.exec(crate::combat::Q::Heal(2));
        }
        if self.has_relic(BagOfPreparation) {
            self.draw_cards(2);
        }
        if self.has_relic(Anchor) {
            self.player.creature.block += 10;
        }
        if self.has_relic(BagOfMarbles) {
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    self.exec(crate::combat::Q::ApplyPowerToMonster {
                        target: mi,
                        kind: PowerKind::Vulnerable,
                        amount: 1,
                    });
                }
            }
        }
        if self.has_relic(RedMask) {
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    self.exec(crate::combat::Q::ApplyPowerToMonster {
                        target: mi,
                        kind: PowerKind::Weak,
                        amount: 1,
                    });
                }
            }
        }
        if self.has_relic(PhilosopherStone) {
            // m.addPower 直加（不走 ApplyPowerAction；力量非减益，神器不拦截）
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    apply_power(
                        &mut self.monsters[mi].creature.powers,
                        PowerKind::Strength,
                        1,
                        false,
                        self.turn_has_ended,
                    );
                }
            }
        }
        if self.has_relic(OddlySmoothStone) {
            self.apply_power_to_player_checked(PowerKind::Dexterity, 1, false);
        }
        if self.has_relic(TwistedFunnel) {
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    self.exec(crate::combat::Q::ApplyPowerToMonster {
                        target: mi,
                        kind: PowerKind::Poison,
                        amount: 4,
                    });
                }
            }
        }
        if self.has_relic(ClockworkSouvenir) {
            self.apply_power_to_player_checked(PowerKind::Artifact, 1, false);
        }
        if self.has_relic(DataDisk) {
            self.apply_power_to_player_checked(PowerKind::Focus, 1, false);
        }
        if self.has_relic(SnakeRing) {
            self.draw_cards(2);
        }
        if self.has_relic(TeardropLocket) {
            self.exec(crate::combat::Q::ChangeStance(Stance::Calm));
        }
        if self.has_relic(MarkOfPain) {
            let wound = CardInst::new("Wound");
            for _ in 0..2 {
                self.exec(crate::combat::Q::MakeInDraw {
                    inst: wound,
                    random: true,
                });
            }
        }
        if let Some(n) = self.relic_counter(DuVuDoll) {
            if n > 0 {
                self.apply_power_to_player_checked(PowerKind::Strength, n, false);
            }
        }
        if let Some(n) = self.relic_counter(Girya) {
            if n != 0 {
                self.apply_power_to_player_checked(PowerKind::Strength, n, false);
            }
        }
        if self.relic_counter(PenNib) == Some(9) {
            // atBattleStart：counter==9（上一场战斗结束时恰为 9）→ 直接授能力
            self.apply_power_to_player_checked(PowerKind::PenNib, 1, false);
        }
        // 首轮 atTurnStart（AbstractRoom:245 applyStartOfTurnRelics）
        self.relics_at_turn_start();
    }

    // ── 回合开始（applyStartOfTurnRelics；调用点：loseBlock 后、抽 5 前） ──

    pub(crate) fn relics_at_turn_start(&mut self) {
        let first_turn = self.turn == 1;
        if first_turn && self.has_relic(Lantern) {
            self.player.energy += 1;
        }
        if self.has_relic(ArtOfWar) {
            // 首回合 firstTurn 不给；此后上回合未打攻击（counter==0）→ +1 能量
            if !first_turn && self.relic_counter(ArtOfWar) == Some(0) {
                self.player.energy += 1;
            }
            self.set_relic_counter(ArtOfWar, 0);
        }
        if self.has_relic(HappyFlower) {
            let c = match self.relic_counter(HappyFlower) {
                Some(-1) => 1, // counter==-1 ? (counter+=2)==1 : ++counter
                Some(c) => c + 1,
                None => 0,
            };
            if c == 3 {
                self.set_relic_counter(HappyFlower, 0);
                self.player.energy += 1;
            } else {
                self.set_relic_counter(HappyFlower, c);
            }
        }
        // 计数器清零类（atTurnStart counter=0）
        for kind in [Shuriken, Kunai, OrnamentalFan, LetterOpener] {
            if self.has_relic(kind) {
                self.set_relic_counter(kind, 0);
            }
        }
        if self.has_relic(HornCleat) {
            let c = self.relic_counter(HornCleat).unwrap_or(0);
            if c != -1 {
                let c = c + 1;
                if c == 2 {
                    self.player.creature.block += 14;
                    self.set_relic_counter(HornCleat, -1);
                } else {
                    self.set_relic_counter(HornCleat, c);
                }
            }
        }
        if self.has_relic(CaptainsWheel) {
            let c = self.relic_counter(CaptainsWheel).unwrap_or(0);
            if c != -1 {
                let c = c + 1;
                if c == 3 {
                    self.player.creature.block += 18;
                    self.set_relic_counter(CaptainsWheel, -1);
                } else {
                    self.set_relic_counter(CaptainsWheel, c);
                }
            }
        }
        if self.has_relic(IncenseBurner) {
            let c = match self.relic_counter(IncenseBurner) {
                Some(-1) => 1,
                Some(c) => c + 1,
                None => 0,
            };
            if c == 6 {
                self.set_relic_counter(IncenseBurner, 0);
                self.apply_power_to_player_checked(PowerKind::IntangiblePlayer, 1, false);
            } else {
                self.set_relic_counter(IncenseBurner, c);
            }
        }
        if self.has_relic(Inserter) {
            let c = match self.relic_counter(Inserter) {
                Some(-1) => 1,
                Some(c) => c + 1,
                None => 0,
            };
            if c == 2 {
                self.set_relic_counter(Inserter, 0);
                self.player.max_orbs += 1;
            } else {
                self.set_relic_counter(Inserter, c);
            }
        }
        if self.has_relic(RunicCapacitor) && first_turn {
            self.player.max_orbs += 3;
        }
        if self.has_relic(StoneCalendar) {
            let c = self.relic_counter(StoneCalendar).unwrap_or(0) + 1;
            self.set_relic_counter(StoneCalendar, c);
        }
        if self.has_relic(MercuryHourglass) {
            self.relic_thorns_damage_all(3);
        }
        if self.has_relic(Brimstone) {
            self.apply_power_to_player_checked(PowerKind::Strength, 2, false);
            for mi in 0..self.monsters.len() {
                if self.alive(mi) {
                    apply_power(
                        &mut self.monsters[mi].creature.powers,
                        PowerKind::Strength,
                        1,
                        false,
                        self.turn_has_ended,
                    );
                }
            }
        }
    }

    // ── 出牌（onUseCard；效果追加到卡牌动作队列尾部 = 卡牌效果之后结算） ──

    pub(crate) fn relics_on_use_card(&mut self, card_type: &str, q: &mut Vec<crate::combat::Q>) {
        use crate::combat::Q;
        match card_type {
            "ATTACK" => {
                if self.has_relic(ArtOfWar) {
                    self.set_relic_counter(ArtOfWar, 1);
                }
                if self.has_relic(Shuriken) {
                    let c = self.relic_counter(Shuriken).unwrap_or(0) + 1;
                    if c % 3 == 0 {
                        self.set_relic_counter(Shuriken, 0);
                        q.push(Q::ApplyPowerToSelf {
                            kind: PowerKind::Strength,
                            amount: 1,
                        });
                    } else {
                        self.set_relic_counter(Shuriken, c);
                    }
                }
                if self.has_relic(Kunai) {
                    let c = self.relic_counter(Kunai).unwrap_or(0) + 1;
                    if c % 3 == 0 {
                        self.set_relic_counter(Kunai, 0);
                        q.push(Q::ApplyPowerToSelf {
                            kind: PowerKind::Dexterity,
                            amount: 1,
                        });
                    } else {
                        self.set_relic_counter(Kunai, c);
                    }
                }
                if self.has_relic(OrnamentalFan) {
                    let c = self.relic_counter(OrnamentalFan).unwrap_or(0) + 1;
                    if c % 3 == 0 {
                        self.set_relic_counter(OrnamentalFan, 0);
                        q.push(Q::PlayerBlock(4));
                    } else {
                        self.set_relic_counter(OrnamentalFan, c);
                    }
                }
                if self.has_relic(Nunchaku) {
                    let c = self.relic_counter(Nunchaku).unwrap_or(0) + 1;
                    if c % 10 == 0 {
                        self.set_relic_counter(Nunchaku, 0);
                        q.push(Q::GainEnergy(1));
                    } else {
                        self.set_relic_counter(Nunchaku, c);
                    }
                }
                if self.has_relic(PenNib) {
                    let c = self.relic_counter(PenNib).unwrap_or(0) + 1;
                    if c == 10 {
                        self.set_relic_counter(PenNib, 0);
                    } else {
                        if c == 9 {
                            // 第 9 张攻击后：授 PenNibPower（下一张攻击 ×2）
                            q.push(Q::ApplyPowerToSelf {
                                kind: PowerKind::PenNib,
                                amount: 1,
                            });
                        }
                        self.set_relic_counter(PenNib, c);
                    }
                }
            }
            "SKILL" => {
                if self.has_relic(LetterOpener) {
                    let c = self.relic_counter(LetterOpener).unwrap_or(0) + 1;
                    if c % 3 == 0 {
                        self.set_relic_counter(LetterOpener, 0);
                        q.push(Q::DamageAllThorns { dmg: 5 });
                    } else {
                        self.set_relic_counter(LetterOpener, c);
                    }
                }
            }
            "POWER" => {
                if self.has_relic(BirdFacedUrn) {
                    q.push(Q::Heal(2));
                }
            }
            _ => {}
        }
    }

    // ── 回合结束（applyEndOfTurnRelics；调用点：end_turn 最前） ──

    pub(crate) fn relics_on_player_end_turn(&mut self) {
        if self.has_relic(Orichalcum) && self.player.creature.block == 0 {
            self.player.creature.block += 6;
        }
        if self.has_relic(StoneCalendar) && self.relic_counter(StoneCalendar) == Some(7) {
            self.relic_thorns_damage_all(52);
        }
        if self.has_relic(FrozenCore) && (self.player.orbs.len() as i32) < self.player.max_orbs {
            self.exec(crate::combat::Q::Channel {
                kind: crate::state::OrbKind::Frost,
                auto_evoke: true,
            });
        }
    }

    // ── 洗牌（onShuffle：ShuffleAction / EmptyDeckShuffleAction；开局洗牌不算） ──

    pub(crate) fn relics_on_shuffle(&mut self) {
        if self.has_relic(Sundial) {
            let c = self.relic_counter(Sundial).unwrap_or(0) + 1;
            if c == 3 {
                self.set_relic_counter(Sundial, 0);
                self.player.energy += 2;
            } else {
                self.set_relic_counter(Sundial, c);
            }
        }
    }

    // ── 怪物死亡（onMonsterDeath；on_monster_death 中于终局判定前调用） ──

    pub(crate) fn relics_on_monster_death(&mut self) {
        if self.has_relic(GremlinHorn) && !self.all_monsters_dead() {
            self.player.energy += 1;
            self.draw_cards(1);
        }
    }

    // ── 消耗（onExhaust；CardGroup.moveToExhaustPile：遗物先于 power） ──

    pub(crate) fn relics_on_exhaust(&mut self) {
        if self.has_relic(CharonsAshes) {
            self.relic_thorns_damage_all(3);
        }
    }

    // ── 受击/失血收尾（AbstractPlayer.damage:1392-1429 玩家侧 relic 管线） ──

    /// Torii.onAttacked（怪物来源 NORMAL 受击，格挡后 2..=5 → 1）→
    /// TungstenRod.onLoseHpLast（每次失血 -1）→ 扣血 → CentennialPuzzle.wasHPLost
    /// （本战斗首次实际掉血抽 3）。返回实际 HP 损失（≥0）。
    pub(crate) fn relic_damage_hooks(&mut self, dmg: i32, ty: DamageType) -> i32 {
        let mut d = dmg.max(0);
        if ty == DamageType::Normal && d > 1 && d <= 5 && self.has_relic(Torii) {
            d = 1;
        }
        if d > 0 && self.has_relic(TungstenRod) {
            d -= 1;
        }
        self.player.creature.hp -= d;
        if d > 0 && self.relic_counter(CentennialPuzzle) == Some(0) {
            self.set_relic_counter(CentennialPuzzle, 1);
            self.draw_cards(3);
        }
        d
    }

    // ── 战斗胜利（AbstractRoom endBattle → relics.onVictory） ──

    /// 胜利结算：BurningBlood/BlackBlood 回血（直接写入玩家 HP）、各遗物
    /// onVictory 计数器复位（Shuriken 系 → -1、HornCleat 系复位于下一场
    /// atBattleStart，此处一并归位）。返回局外层应结算的最大生命增量
    /// （FaceOfCleric +1）。
    pub fn on_victory(&mut self) -> i32 {
        let mut max_hp_gain = 0;
        if self.has_relic(BurningBlood) && self.player.creature.hp > 0 {
            self.player.creature.hp =
                (self.player.creature.hp + 6).min(self.player.creature.max_hp);
        }
        if self.has_relic(BlackBlood) && self.player.creature.hp > 0 {
            self.player.creature.hp =
                (self.player.creature.hp + 12).min(self.player.creature.max_hp);
        }
        if self.has_relic(FaceOfCleric) {
            max_hp_gain += 1;
        }
        for kind in [
            Shuriken,
            Kunai,
            OrnamentalFan,
            LetterOpener,
            HornCleat,
            CaptainsWheel,
            StoneCalendar,
        ] {
            if self.has_relic(kind) {
                self.set_relic_counter(kind, -1);
            }
        }
        max_hp_gain
    }
}

