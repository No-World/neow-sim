//! 事件系统（EventRoom + AbstractDungeon.generateEvent 逐行对照，第一批：
//! Act1 全部事件 + 全幕通用神龛/一次性事件；Act2/3 专属事件与城市专属一次性
//! 事件留待后续批次，未实现者在池构建时过滤并标注偏差）。
//!
//! 源码对照：
//! - `EventRoom.onPlayerEntry` → `generateEvent(eventRng)`：
//!   `rng.random(1.0) < 0.25`（shrineChance）→ getShrine，否则 getEvent
//!   （eventList 空 → 回退 getShrine）
//! - `getEvent`：按条件过滤 eventList（Dead Adventurer / Mushrooms 需 floor>6；
//!   The Cleric 需金币≥35），uniform `rng.random(size-1)` 抽取后**从池移除**
//!   （幕内不重复）
//! - `getShrine`：shrineList + 条件过滤后的一次性事件（Fountain of Cleansing
//!   需有诅咒；The Woman in Blue 需金币≥50；FaceTrader 仅幕 1/2），uniform 抽取
//!   后从两池同时移除
//! - 事件效果数值逐行对照各事件类（BigFish/Cleric/... buttonEffect）
//!
//! ⚠️ 残留偏差：
//! - 源码事件池含未实现事件（Match and Keep! 小游戏、Act2/3 事件、城市专属
//!   一次性事件、SecretPortal 时长门槛），本批从池中过滤——事件房间出现频率
//!   略高于原版（可选池变小）
//! - DeadAdventurer 奖励袋洗牌：源码 `Collections.shuffle(new
//!   Random(miscRng.randomLong()))`（JavaRandom 种子化），引擎以 miscRng
//!   Fisher-Yates 等价替代（概率分布一致，消耗序列不同）
//! - Mushrooms 战斗（The Mushroom Lair）：Mushroomer 行为层未实现，选项按
//!   「直接获得战利」结算（金币 + Odd Mushroom 遗物，无卡牌奖励/无战斗风险）
//! - FaceTrader 交易面具（CultistMask 等未入遗物池）：以已实现池随机遗物替代
//! - 遗物 tier 骰照掷（保持 relicRng 消耗序列），具体遗物从已实现池取
//! - WeMeetAgain「送卡」可选任意牌（源码排除初始打击/防御）

use crate::cards::CardInst;
use crate::combat::Combat;
use crate::generated::CARDS;
use crate::potions::PotionKind;
use crate::relics::{RelicInst, RelicKind};
use crate::run::{RewardItem, RunState, MAP_HEIGHT};
use crate::state::MonsterKind;

/// 事件种类（第一批：Act1 全量 + 通用神龛 + Act1 可见的一次性事件）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    // ── Act1 事件（Exordium.initializeEventList 顺序）──
    BigFish,
    Cleric,
    DeadAdventurer,
    GoldenIdol,
    GoldenWing,
    GoopPuddle,
    LiarsGame,
    LivingWall,
    Mushrooms,
    ScrapOoze,
    ShiningLight,
    // ── 神龛（全幕 shrineList；Match and Keep! 未实现不入池）──
    GoldShrine,
    Transmogrifier,
    PurificationShrine,
    UpgradeShrine,
    WheelOfChange,
    // ── 一次性事件（specialOneTimeEventList；城市/尾幕/门槛项未实现不入池）──
    AccursedBlacksmith,
    Bonfire,
    FaceTrader,
    FountainOfCurseRemoval,
    Lab,
    WeMeetAgain,
    WomanInBlue,
    // ── Act2 事件（TheCity.initializeEventList；CursedTome 未入池：
    // Necronomicon 卡不在数据提取范围）──
    Addict,
    BackToBasics,
    Beggar,
    Colosseum,
    DrugDealer,
    ForgottenAltar,
    Ghosts,
    MaskedBandits,
    Nest,
    TheLibrary,
    TheMausoleum,
    Vampires,
    // ── Act3 事件（TheBeyond.initializeEventList）──
    Falling,
    MindBloom,
    MysteriousSphere,
    SensoryStone,
    TombRedMask,
    WindingHalls,
    // ── 城市专属一次性事件（specialOneTimeEventList）──
    Designer,
    Duplicator,
    KnowingSkull,
    Nloth,
    TheJoust,
}

impl EventKind {
    pub fn id(&self) -> &'static str {
        use EventKind as E;
        match self {
            E::BigFish => "Big Fish",
            E::Cleric => "The Cleric",
            E::DeadAdventurer => "Dead Adventurer",
            E::GoldenIdol => "Golden Idol",
            E::GoldenWing => "Golden Wing",
            E::GoopPuddle => "World of Goop",
            E::LiarsGame => "Liars Game",
            E::LivingWall => "Living Wall",
            E::Mushrooms => "Mushrooms",
            E::ScrapOoze => "Scrap Ooze",
            E::ShiningLight => "Shining Light",
            E::GoldShrine => "Golden Shrine",
            E::Transmogrifier => "Transmorgrifier",
            E::PurificationShrine => "Purifier",
            E::UpgradeShrine => "Upgrade Shrine",
            E::WheelOfChange => "Wheel of Change",
            E::AccursedBlacksmith => "Accursed Blacksmith",
            E::Bonfire => "Bonfire Elementals",
            E::FaceTrader => "FaceTrader",
            E::FountainOfCurseRemoval => "Fountain of Cleansing",
            E::Lab => "Lab",
            E::WeMeetAgain => "WeMeetAgain",
            E::WomanInBlue => "The Woman in Blue",
            E::Addict => "Addict",
            E::BackToBasics => "Back to Basics",
            E::Beggar => "Beggar",
            E::Colosseum => "Colosseum",
            E::DrugDealer => "DrugDealer",
            E::ForgottenAltar => "Forgotten Altar",
            E::Ghosts => "Ghosts",
            E::MaskedBandits => "Masked Bandits",
            E::Nest => "Nest",
            E::TheLibrary => "The Library",
            E::TheMausoleum => "The Mausoleum",
            E::Vampires => "Vampires",
            E::Falling => "Falling",
            E::MindBloom => "MindBloom",
            E::MysteriousSphere => "Mysterious Sphere",
            E::SensoryStone => "SensoryStone",
            E::TombRedMask => "Tomb of Lord Red Mask",
            E::WindingHalls => "Winding Halls",
            E::Designer => "Designer",
            E::Duplicator => "Duplicator",
            E::KnowingSkull => "Knowing Skull",
            E::Nloth => "N'loth",
            E::TheJoust => "The Joust",
        }
    }
}

/// 事件内选牌的待结算效果（gridSelect 协议层呈现，引擎在
/// `event_pick_card` 结算）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingPick {
    /// 删卡（Living Wall 遗忘 / 净化神龛 / 牧师净化 / 命运转轮 / Cleric）
    Remove,
    /// 变化（Living Wall 变化 / 变形神龛）
    Transform,
    /// 升级（Living Wall 成长 / 升级神龛 / 邪恶铁匠锻造）
    Upgrade,
    /// 送卡换遗物（WeMeetAgain）
    GiveCard,
    /// 献祭（Bonfire：按稀有度结算）
    BonfireOffer,
    /// 拆翼（GoldenWing：删卡 + 7 伤）
    GoldenWingRemove,
    /// 复制（Duplicator：卡组加一份副本）
    Duplicate,
}

/// 事件实例状态（screen：0=主屏，1=结果屏；done=结果屏已确认可离开；
/// 多屏事件（GoldenIdol 0/1/2、Colosseum 0 入口/1 波间/2 结果）按 kind 自定义）
#[derive(Clone, Debug)]
pub struct EventState {
    pub kind: EventKind,
    pub screen: u8,
    pub done: bool,
    /// 预计算治疗量（BigFish / Cleric / Mushrooms / WindingHalls）
    pub heal_amt: i32,
    /// 预计算伤害（GoldenWing / GoopPuddle / ShiningLight / ScrapOoze /
    /// FaceTrader / ForgottenAltar / WindingHalls）
    pub damage: i32,
    /// GoldenIdol / Ghosts / Vampires / WindingHalls 最大生命损失
    pub maxhp_loss: i32,
    /// 预计算金币（GoldShrine / Wheel / GoopPuddle / LiarsGame / FaceTrader /
    /// WeMeetAgain / Nest / MindBloom）
    pub gold: i32,
    /// GoopPuddle 金币损失 / Cleric 净化价 / Designer 各档价（借位）
    pub gold_loss: i32,
    /// DeadAdventurer 遭遇概率 / ScrapOoze 遗物概率
    pub chance: i32,
    /// DeadAdventurer 已领奖励数 / Colosseum 波次 / KnowingSkull 已购数
    pub counter: i32,
    /// DeadAdventurer 敌人（0=3 Sentries / 1=Gremlin Nob / 2=Lagavulin）
    pub enemy: i32,
    /// DeadAdventurer 奖励袋 / Falling 预选三卡下标 / Nloth 预选两只遗物下标
    pub bag: Vec<u8>,
    /// 事件内选牌待结算队列（ Designer 全套服务两步 / DrugDealer 变化两张）
    pub pending: Vec<PendingPick>,
    /// TheLibrary 20 张候选
    pub library: Vec<CardInst>,
    /// KnowingSkull 三档 HP 价（ potion / gold / card；离开恒 6）
    pub costs: [i32; 3],
}

/// 协议层选项描述
#[derive(Clone, Debug)]
pub struct EventOption {
    pub tag: &'static str,
    pub label: String,
    pub disabled: bool,
}

/// Act 事件池（Exordium.initializeEventList；Act2/3 未实现 → 空，事件房回退神龛）
pub fn act_event_pool(act: u8) -> Vec<EventKind> {
    use EventKind as E;
    match act {
        1 => vec![
            E::BigFish,
            E::Cleric,
            E::DeadAdventurer,
            E::GoldenIdol,
            E::GoldenWing,
            E::GoopPuddle,
            E::LiarsGame,
            E::LivingWall,
            E::Mushrooms,
            E::ScrapOoze,
            E::ShiningLight,
        ],
        // ⚠️ 偏差：CursedTome 未入池（Necronomicon 卡不在数据提取范围）
        2 => vec![
            E::Addict,
            E::BackToBasics,
            E::Beggar,
            E::Colosseum,
            E::DrugDealer,
            E::ForgottenAltar,
            E::Ghosts,
            E::MaskedBandits,
            E::Nest,
            E::TheLibrary,
            E::TheMausoleum,
            E::Vampires,
        ],
        3 => vec![
            E::Falling,
            E::MindBloom,
            E::MysteriousSphere,
            E::SensoryStone,
            E::TombRedMask,
            E::WindingHalls,
        ],
        _ => Vec::new(),
    }
}

/// 神龛池（initializeShrineList；Match and Keep! 小游戏未实现）
pub fn shrine_pool() -> Vec<EventKind> {
    use EventKind as E;
    vec![
        E::GoldShrine,
        E::Transmogrifier,
        E::PurificationShrine,
        E::UpgradeShrine,
        E::WheelOfChange,
    ]
}

/// 一次性事件池（initializeSpecialOneTimeEventList 已实现子集：
/// 城市专属 5 只本批接入；SecretPortal 时长门槛 / NoteForYourself 存档门槛未入池）
pub fn special_pool() -> Vec<EventKind> {
    use EventKind as E;
    vec![
        E::AccursedBlacksmith,
        E::Bonfire,
        E::FaceTrader,
        E::FountainOfCurseRemoval,
        E::Lab,
        E::WeMeetAgain,
        E::WomanInBlue,
        E::Designer,
        E::Duplicator,
        E::KnowingSkull,
        E::Nloth,
        E::TheJoust,
    ]
}

/// 不可净化诅咒（getPurgeableCards 排除表）
const UNPURGEABLE: &[&str] = &["AscendersBane", "CurseOfTheBell", "Necronomicurse"];

impl RunState {
    // ── 池与生成 ──────────────────────────────────────────────

    /// populate_act 时重建事件池（幕内不重复；一次性事件池随幕重建——
    /// 对照源码每次进幕 initializeEventList/initializeShrineList/
    /// initializeSpecialOneTimeEventList 均重填）
    pub fn init_event_pools(&mut self) {
        self.event_pool = act_event_pool(self.act);
        self.shrine_pool = shrine_pool();
        self.special_pool = special_pool();
    }

    /// generateEvent：eventRng.random(1.0) < 0.25 → 神龛，否则事件（空回退神龛）
    pub fn enter_event(&mut self) {
        let shrine_first = self.rng.event.random_float() < 0.25;
        let kind = if shrine_first {
            self.pick_shrine()
        } else {
            self.pick_event().or_else(|| self.pick_shrine())
        };
        let Some(kind) = kind else { return };
        let ev = EventState {
            kind,
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
            pending: Vec::new(),
            library: Vec::new(),
            costs: [0; 3],
        };
        self.event = Some(self.construct_event(ev));
    }

    /// getEvent：条件过滤 + uniform 抽取 + 池移除
    fn pick_event(&mut self) -> Option<EventKind> {
        use EventKind as E;
        let floor_ok = self.floor > 6;
        let later_half = self.pos.0 > MAP_HEIGHT / 2; // Colosseum：地图后半
        let mut tmp: Vec<EventKind> = self
            .event_pool
            .iter()
            .copied()
            .filter(|k| match k {
                E::DeadAdventurer | E::Mushrooms => floor_ok,
                E::Cleric => self.player.gold >= 35,
                E::Beggar => self.player.gold >= 75,
                E::Colosseum => later_half,
                _ => true,
            })
            .collect();
        if tmp.is_empty() {
            return None;
        }
        let idx =
            (self.rng.event.random_range(tmp.len() as i32 - 1).max(0) as usize).min(tmp.len() - 1);
        let kind = tmp.remove(idx);
        self.event_pool.retain(|k| *k != kind);
        Some(kind)
    }

    /// getShrine：神龛 + 条件过滤后的一次性事件，uniform 抽取后两池移除
    fn pick_shrine(&mut self) -> Option<EventKind> {
        use EventKind as E;
        let has_curse = self.deck.iter().any(|c| c.def.card_type == "CURSE");
        let mut tmp: Vec<EventKind> = self.shrine_pool.clone();
        for k in &self.special_pool {
            let ok = match k {
                E::FountainOfCurseRemoval => has_curse,
                E::WomanInBlue => self.player.gold >= 50,
                E::FaceTrader => self.act <= 2,
                // 城市专属一次性事件（源码条件逐行）
                E::Designer => self.act >= 2 && self.player.gold >= 75,
                E::Duplicator => self.act >= 2,
                E::KnowingSkull => self.act == 2 && self.player.hp > 12,
                E::Nloth => self.act == 2 && self.relics.len() >= 2,
                E::TheJoust => self.act == 2 && self.player.gold >= 50,
                _ => true,
            };
            if ok {
                tmp.push(*k);
            }
        }
        if tmp.is_empty() {
            return None;
        }
        let idx =
            (self.rng.event.random_range(tmp.len() as i32 - 1).max(0) as usize).min(tmp.len() - 1);
        let kind = tmp.remove(idx);
        self.shrine_pool.retain(|k| *k != kind);
        self.special_pool.retain(|k| *k != kind);
        Some(kind)
    }

    /// 事件构造（消耗构造期 rng：GoopPuddle/DeadAdventurer/WeMeetAgain）
    fn construct_event(&mut self, mut ev: EventState) -> EventState {
        let max_hp = self.player.max_hp;
        let asc = self.ascension;
        match ev.kind {
            EventKind::BigFish => ev.heal_amt = max_hp / 3,
            EventKind::Cleric => {
                ev.heal_amt = (max_hp as f32 * 0.25) as i32;
                ev.gold_loss = if asc >= 15 { 75 } else { 50 };
            }
            EventKind::DeadAdventurer => {
                ev.bag = vec![0, 1, 2];
                // ⚠️ 偏差：Fisher-Yates（miscRng）替代 JavaRandom 种子洗牌
                for i in (1..ev.bag.len()).rev() {
                    let j = self.rng.misc.random_range(i as i32) as usize;
                    ev.bag.swap(i, j);
                }
                ev.chance = if asc >= 15 { 35 } else { 25 };
                ev.enemy = self.rng.misc.random_start_end(0, 2).clamp(0, 2);
            }
            EventKind::GoldenIdol => {
                ev.damage = (max_hp as f32 * if asc >= 15 { 0.35 } else { 0.25 }) as i32;
                ev.maxhp_loss =
                    (((max_hp as f32) * if asc >= 15 { 0.10 } else { 0.08 }) as i32).max(1);
            }
            EventKind::GoldenWing => ev.damage = 7,
            EventKind::GoopPuddle => {
                ev.damage = 11;
                ev.gold = 75;
                let loss = if asc >= 15 {
                    self.rng.misc.random_start_end(35, 75)
                } else {
                    self.rng.misc.random_start_end(20, 50)
                };
                ev.gold_loss = loss.min(self.player.gold);
            }
            EventKind::LiarsGame => ev.gold = if asc >= 15 { 150 } else { 175 },
            EventKind::Mushrooms => ev.heal_amt = (max_hp as f32 * 0.25) as i32,
            EventKind::ScrapOoze => {
                ev.chance = 25;
                ev.damage = if asc >= 15 { 5 } else { 3 };
            }
            EventKind::ShiningLight => {
                let pct = if asc >= 15 { 0.3 } else { 0.2 };
                ev.damage = ((max_hp as f32) * pct + 0.5) as i32; // MathUtils.round
            }
            EventKind::GoldShrine => ev.gold = if asc >= 15 { 50 } else { 100 },
            EventKind::WheelOfChange => {
                ev.gold = match self.act {
                    1 => 100,
                    2 => 200,
                    _ => 300,
                };
            }
            EventKind::FaceTrader => {
                ev.gold = if asc >= 15 { 50 } else { 75 };
                ev.damage = (max_hp / 10).max(1);
            }
            EventKind::WeMeetAgain => {
                // getGoldAmount（构造期消耗 miscRng）
                ev.gold = if self.player.gold < 50 {
                    0
                } else if self.player.gold > 150 {
                    self.rng.misc.random_start_end(50, 150)
                } else {
                    self.rng.misc.random_start_end(50, self.player.gold)
                };
            }
            EventKind::ForgottenAltar => {
                ev.damage = ((max_hp as f32) * if asc >= 15 { 0.35 } else { 0.25 } + 0.5) as i32;
            }
            EventKind::Ghosts => {
                ev.maxhp_loss = ((max_hp as f32 * 0.5).ceil() as i32).min(max_hp - 1);
            }
            EventKind::Vampires => {
                ev.maxhp_loss = ((max_hp as f32 * 0.3).ceil() as i32).min(max_hp - 1);
            }
            EventKind::Nest => ev.gold = if asc >= 15 { 50 } else { 99 },
            EventKind::WindingHalls => {
                let (hp_pct, heal_pct) = if asc >= 15 {
                    (0.18, 0.20)
                } else {
                    (0.125, 0.25)
                };
                ev.damage = ((max_hp as f32) * hp_pct + 0.5) as i32;
                ev.heal_amt = ((max_hp as f32) * heal_pct + 0.5) as i32;
                ev.maxhp_loss = ((max_hp as f32) * 0.05 + 0.5) as i32;
            }
            EventKind::MindBloom => ev.gold = if asc >= 13 { 25 } else { 50 },
            EventKind::Falling => {
                // setCards：各类型从自己牌组随机预选一张（miscRng）；
                // bag = [skill, power, attack] 下标（255 饱和 = 无该类型）
                ev.bag = ["SKILL", "POWER", "ATTACK"]
                    .iter()
                    .map(|ty| {
                        let idxs: Vec<usize> = self
                            .deck
                            .iter()
                            .enumerate()
                            .filter(|(_, c)| c.def.card_type == *ty)
                            .map(|(i, _)| i)
                            .collect();
                        if idxs.is_empty() {
                            255
                        } else {
                            let roll =
                                self.rng.misc.random_range(idxs.len() as i32 - 1).max(0) as usize;
                            idxs[roll.min(idxs.len() - 1)].min(255) as u8
                        }
                    })
                    .collect();
            }
            EventKind::Nloth => {
                // 洗牌选 2（偏差：miscRng 无放回；源码 JavaRandom 种子洗牌）
                let n = self.relics.len() as i32;
                let a = self.rng.misc.random_range(n - 1).max(0);
                let mut b = self.rng.misc.random_range(n - 1).max(0);
                if b == a {
                    b = (b + 1) % n.max(1);
                }
                ev.bag = vec![a as u8, b as u8];
            }
            EventKind::Designer => {
                // 构造期两枚布尔（miscRng）+ 价目（A15 上调）
                let upg_one = self.rng.misc.random_boolean();
                let rm_cards = self.rng.misc.random_boolean();
                let (adj, clean, full, hp) = if asc >= 15 {
                    (50, 75, 110, 5)
                } else {
                    (40, 60, 90, 3)
                };
                ev.chance = if upg_one { 1 } else { 0 };
                ev.counter = if rm_cards { 1 } else { 0 };
                ev.gold = adj;
                ev.gold_loss = clean;
                ev.maxhp_loss = full;
                ev.damage = hp;
            }
            EventKind::KnowingSkull => ev.costs = [6, 6, 6],
            _ => {}
        }
        ev
    }

    // ── 查询 ──────────────────────────────────────────────

    /// 主牌组可净化卡（getPurgeableCards：排除不可净化诅咒）
    pub fn deck_purgeable(&self) -> Vec<usize> {
        self.deck
            .iter()
            .enumerate()
            .filter(|(_, c)| !UNPURGEABLE.contains(&c.def.id))
            .map(|(i, _)| i)
            .collect()
    }

    /// 主牌组可升级卡（canUpgrade = 未升级）
    pub fn deck_upgradable(&self) -> Vec<usize> {
        self.deck
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.upgraded)
            .map(|(i, _)| i)
            .collect()
    }

    /// 当前事件选项（协议层渲染用；disabled 项禁止选择）
    pub fn event_options(&self) -> Vec<EventOption> {
        match &self.event {
            Some(ev) => self.options_for(ev),
            None => Vec::new(),
        }
    }

    /// 指定事件状态的选项表（choose 复用：take 后仍可取禁用态）
    fn options_for(&self, ev: &EventState) -> Vec<EventOption> {
        use EventKind as E;
        // 小闭包：字面量/format 均可（label 统一 .into()）
        fn o(tag: &'static str, label: impl Into<String>, disabled: bool) -> EventOption {
            EventOption {
                tag,
                label: label.into(),
                disabled,
            }
        }
        match ev.kind {
            E::BigFish => vec![
                o(
                    "banana",
                    format!("[Banana] Heal {} HP.", ev.heal_amt),
                    false,
                ),
                o("donut", "[Donut] Max HP +5.", false),
                o("box", "[Box] Gain a relic. Obtain Regret.", false),
            ],
            E::Cleric => vec![
                o(
                    "heal",
                    format!("[Heal] Lose 35 gold. Heal {} HP.", ev.heal_amt),
                    self.player.gold < 35,
                ),
                o(
                    "purify",
                    format!("[Purify] Remove a card. Lose {} gold.", ev.gold_loss),
                    self.player.gold < ev.gold_loss || self.deck_purgeable().is_empty(),
                ),
                o("leave", "[Leave]", false),
            ],
            E::DeadAdventurer => {
                if ev.screen == 0 {
                    vec![
                        o(
                            "search",
                            format!("[Search] {}% chance of a fight.", ev.chance),
                            false,
                        ),
                        o("escape", "[Escape]", false),
                    ]
                } else {
                    vec![o("leave", "[Leave]", false)]
                }
            }
            E::GoldenIdol => match ev.screen {
                0 => vec![
                    o("idol", "[Take Idol] Obtain Golden Idol.", false),
                    o("leave", "[Leave]", false),
                ],
                1 => vec![
                    o("injury", "[Grab] Obtain Injury.", false),
                    o(
                        "damage",
                        format!("[Fight] Take {} damage.", ev.damage),
                        false,
                    ),
                    o(
                        "maxhp",
                        format!("[Flatten] Lose {} Max HP.", ev.maxhp_loss),
                        false,
                    ),
                ],
                _ => vec![o("leave", "[Leave]", false)],
            },
            E::GoldenWing => vec![
                o(
                    "reach",
                    format!("[Reach] Take {} damage. Gain 50-80 gold.", ev.damage),
                    false,
                ),
                o(
                    "pry",
                    format!("[Pry] Remove a card. Take {} damage.", ev.damage),
                    self.deck_purgeable().is_empty(),
                ),
            ],
            E::GoopPuddle => vec![
                o(
                    "gather",
                    format!("[Gather] Take {} damage. Gain {} gold.", ev.damage, ev.gold),
                    false,
                ),
                o(
                    "leave",
                    format!("[Leave] Lose {} gold.", ev.gold_loss),
                    false,
                ),
            ],
            E::LiarsGame => vec![
                o(
                    "agree",
                    format!("[Agree] Gain {} gold. Obtain Doubt.", ev.gold),
                    false,
                ),
                o("refuse", "[Refuse]", false),
            ],
            E::LivingWall => vec![
                o(
                    "forget",
                    "[Forget] Remove a card.",
                    self.deck_purgeable().is_empty(),
                ),
                o(
                    "change",
                    "[Change] Transform a card.",
                    self.deck_purgeable().is_empty(),
                ),
                o(
                    "grow",
                    "[Grow] Upgrade a card.",
                    self.deck_upgradable().is_empty(),
                ),
            ],
            E::Mushrooms => vec![
                o("fight", "[Fight] The Mushroom Lair.", false),
                o(
                    "flee",
                    format!("[Flee] Heal {} HP. Obtain Parasite.", ev.heal_amt),
                    false,
                ),
            ],
            E::ScrapOoze => vec![
                o(
                    "reach",
                    format!(
                        "[Reach] Take {} damage. {}% relic chance.",
                        ev.damage, ev.chance
                    ),
                    false,
                ),
                o("escape", "[Escape]", false),
            ],
            E::ShiningLight => vec![
                o(
                    "jump",
                    format!("[Jump] Take {} damage. Upgrade 2 random cards.", ev.damage),
                    self.deck_upgradable().is_empty(),
                ),
                o("leave", "[Leave]", false),
            ],
            E::GoldShrine => vec![
                o("pray", format!("[Pray] Gain {} gold.", ev.gold), false),
                o(
                    "desecrate",
                    "[Desecrate] Gain 275 gold. Obtain Regret.",
                    false,
                ),
                o("leave", "[Leave]", false),
            ],
            E::Transmogrifier => vec![o(
                "transform",
                "[Transform] Transform a card.",
                self.deck.is_empty(),
            )],
            E::PurificationShrine => vec![o(
                "purify",
                "[Purify] Remove a card.",
                self.deck_purgeable().is_empty(),
            )],
            E::UpgradeShrine => vec![o(
                "upgrade",
                "[Pray] Upgrade a card.",
                self.deck_upgradable().is_empty(),
            )],
            E::WheelOfChange => vec![o("spin", "[Spin]", false)],
            E::AccursedBlacksmith => vec![
                o(
                    "forge",
                    "[Forge] Upgrade a card.",
                    self.deck_upgradable().is_empty(),
                ),
                o(
                    "rummage",
                    "[Rummage] Obtain Pain. Obtain Warped Tongs.",
                    false,
                ),
                o("leave", "[Leave]", false),
            ],
            E::Bonfire => vec![o(
                "offer",
                "[Sacrifice] Remove a card for a blessing.",
                self.deck_purgeable().is_empty(),
            )],
            E::FaceTrader => vec![
                o(
                    "touch",
                    format!("[Touch] Take {} damage. Gain {} gold.", ev.damage, ev.gold),
                    false,
                ),
                o("trade", "[Trade] Gain a relic.", false),
                o("leave", "[Leave]", false),
            ],
            E::FountainOfCurseRemoval => vec![
                o("drink", "[Drink] Remove all curses.", false),
                o("leave", "[Leave]", false),
            ],
            E::Lab => vec![o("consume", "[Consume] Gain random potions.", false)],
            E::WeMeetAgain => {
                let mut v = Vec::new();
                if self.potions.iter().any(|p| p.is_some()) {
                    v.push(o("potion", "[Give Potion] Gain a relic.", false));
                }
                if ev.gold > 0 {
                    v.push(o(
                        "gold",
                        format!("[Give Gold] Lose {} gold. Gain a relic.", ev.gold),
                        self.player.gold < ev.gold,
                    ));
                }
                if !self.deck.is_empty() {
                    v.push(o("card", "[Give Card] Gain a relic.", false));
                }
                v.push(o("leave", "[Leave]", false));
                v
            }
            E::WomanInBlue => vec![
                o("buy1", "[Buy 1] 20 gold.", self.player.gold < 20),
                o("buy2", "[Buy 2] 30 gold.", self.player.gold < 30),
                o("buy3", "[Buy 3] 40 gold.", self.player.gold < 40),
                o(
                    "leave",
                    if self.ascension >= 15 {
                        format!(
                            "[Leave] Take {} damage.",
                            ((self.player.max_hp as f32) * 0.05).ceil() as i32
                        )
                    } else {
                        "[Leave]".to_string()
                    },
                    false,
                ),
            ],
            // ── Act2 ──
            E::Addict => vec![
                o(
                    "buy",
                    "[Buy] Lose 85 gold. Gain a relic.",
                    self.player.gold < 85,
                ),
                o("steal", "[Steal] Gain a relic. Obtain Shame.", false),
                o("leave", "[Leave]", false),
            ],
            E::BackToBasics => vec![
                o(
                    "forget",
                    "[Forget] Remove a card.",
                    self.deck_purgeable().is_empty(),
                ),
                o("grow", "[Grow] Upgrade all Strikes and Defends.", false),
            ],
            E::Beggar => vec![
                o(
                    "give",
                    "[Give] Lose 75 gold. Remove a card.",
                    self.player.gold < 75,
                ),
                o("refuse", "[Refuse]", false),
            ],
            E::Colosseum => match ev.screen {
                0 => vec![o("fight", "[Fight]", false)],
                1 => vec![
                    o("fight", "[Fight] Face the Nobs.", false),
                    o("flee", "[Flee]", false),
                ],
                3 => vec![o("leave", "[Leave]", false)],
                _ => vec![],
            },
            E::DrugDealer => vec![
                o("inject", "[Inject] Gain Mutagenic Strength.", false),
                o(
                    "test",
                    "[Test Subject] Transform 2 cards.",
                    self.deck_purgeable().len() < 2,
                ),
                o("leave", "[Leave]", false),
            ],
            E::ForgottenAltar => {
                let mut v = Vec::new();
                if self.relics.iter().any(|r| r.kind == RelicKind::GoldenIdol) {
                    v.push(o("place", "[Place] Golden Idol → Bloody Idol.", false));
                }
                v.push(o(
                    "bleed",
                    format!("[Bleed] Max HP +5. Take {} damage.", ev.damage),
                    false,
                ));
                v.push(o("smash", "[Smash] Obtain Decay.", false));
                v
            }
            E::Ghosts => vec![
                o(
                    "become",
                    format!(
                        "[Become] Lose {} Max HP. Gain {} Apparitions.",
                        ev.maxhp_loss,
                        if self.ascension >= 15 { 3 } else { 5 }
                    ),
                    false,
                ),
                o("refuse", "[Refuse]", false),
            ],
            E::MaskedBandits => vec![
                o("pay", "[Pay] Lose all your gold.", self.player.gold == 0),
                o("fight", "[Fight]", false),
            ],
            E::Nest => vec![
                o("steal", format!("[Steal] Gain {} gold.", ev.gold), false),
                o("join", "[Join] Take 6 damage. Obtain Ritual Dagger.", false),
            ],
            E::TheLibrary => {
                if ev.screen == 0 {
                    vec![
                        o("read", "[Read] Choose 1 of 20 random cards.", false),
                        o(
                            "sleep",
                            format!(
                                "[Sleep] Heal {} HP.",
                                ((self.player.max_hp as f32)
                                    * if self.ascension >= 15 { 0.2 } else { 0.33 }
                                    + 0.5) as i32
                            ),
                            false,
                        ),
                    ]
                } else if ev.screen == 1 {
                    vec![] // 待 event_library_pick（协议层 grid 选卡）
                } else {
                    vec![o("leave", "[Leave]", false)]
                }
            }
            E::TheMausoleum => vec![
                o("open", "[Open] Gain a relic. 50% chance of a curse.", false),
                o("leave", "[Leave]", false),
            ],
            E::Vampires => {
                let mut v = vec![o(
                    "feed",
                    format!(
                        "[Feed] Lose {} Max HP. Remove all Strikes. Gain 5 Bites.",
                        ev.maxhp_loss
                    ),
                    false,
                )];
                if self.relics.iter().any(|r| r.kind == RelicKind::BloodVial) {
                    v.push(o("vial", "[Blood Vial] Feed without losing Max HP.", false));
                }
                v.push(o("leave", "[Leave]", false));
                v
            }
            // ── Act3 ──
            E::Falling => {
                if ev.screen == 0 {
                    vec![o("brace", "[Brace Yourself]", false)]
                } else if ev.screen == 1 {
                    let name_of = |i: u8| {
                        if i as usize >= self.deck.len() {
                            String::new()
                        } else {
                            self.deck[i as usize].def.id.to_string()
                        }
                    };
                    vec![
                        o(
                            "skill",
                            format!("[Catch] Remove {}.", name_of(ev.bag[0])),
                            ev.bag[0] as usize >= self.deck.len(),
                        ),
                        o(
                            "power",
                            format!("[Catch] Remove {}.", name_of(ev.bag[1])),
                            ev.bag[1] as usize >= self.deck.len(),
                        ),
                        o(
                            "attack",
                            format!("[Catch] Remove {}.", name_of(ev.bag[2])),
                            ev.bag[2] as usize >= self.deck.len(),
                        ),
                    ]
                } else {
                    vec![o("leave", "[Leave]", false)]
                }
            }
            E::MindBloom => vec![
                o("fight", "[Fight] A boss from the past.", false),
                o(
                    "upgrade",
                    "[Upgrade] Upgrade all cards. Gain Mark of the Bloom.",
                    false,
                ),
                o(
                    "gold",
                    if self.floor % 50 <= 40 {
                        "[Gold] Gain 999 gold. Obtain 2 Normality."
                    } else {
                        "[Heal] Fully heal. Obtain Doubt."
                    },
                    false,
                ),
            ],
            E::MysteriousSphere => vec![
                o("fight", "[Touch] Fight 2 Orb Walkers.", false),
                o("leave", "[Leave]", false),
            ],
            E::SensoryStone => vec![
                o("m1", "[Memory] Gain 1 colorless card.", false),
                o(
                    "m2",
                    "[Memory] Take 5 damage. Gain 2 colorless cards.",
                    false,
                ),
                o(
                    "m3",
                    "[Memory] Take 10 damage. Gain 3 colorless cards.",
                    false,
                ),
            ],
            E::TombRedMask => {
                let mut v = Vec::new();
                if self.relics.iter().any(|r| r.kind == RelicKind::RedMask) {
                    v.push(o("take", "[Take] Gain 222 gold.", false));
                } else {
                    v.push(o("take", "[Take] Gain 222 gold.", true));
                    v.push(o(
                        "pay",
                        "[Pay] Lose all gold. Gain Red Mask.",
                        self.player.gold == 0,
                    ));
                }
                v.push(o("leave", "[Leave]", false));
                v
            }
            E::WindingHalls => vec![
                o(
                    "madness",
                    format!("[Embrace] Take {} damage. Gain 2 Madness.", ev.damage),
                    false,
                ),
                o(
                    "writhe",
                    format!("[Enter] Heal {}. Obtain Writhe.", ev.heal_amt),
                    false,
                ),
                o(
                    "flat",
                    format!("[Continue] Lose {} Max HP.", ev.maxhp_loss),
                    false,
                ),
            ],
            // ── 城市专属一次性 ──
            E::Designer => vec![
                o(
                    "adjust",
                    if ev.chance == 1 {
                        format!("[Adjust] {} gold. Upgrade a card.", ev.gold)
                    } else {
                        format!("[Adjust] {} gold. Upgrade 2 random cards.", ev.gold)
                    },
                    self.player.gold < ev.gold || self.deck_upgradable().is_empty(),
                ),
                o(
                    "cleanup",
                    if ev.counter == 1 {
                        format!("[Clean] {} gold. Remove a card.", ev.gold_loss)
                    } else {
                        format!("[Clean] {} gold. Transform 2 cards.", ev.gold_loss)
                    },
                    self.player.gold < ev.gold_loss || self.deck_purgeable().len() < 2,
                ),
                o(
                    "full",
                    format!("[Full Service] {} gold. Clean + Adjust.", ev.maxhp_loss),
                    self.player.gold < ev.maxhp_loss || self.deck_purgeable().is_empty(),
                ),
                o(
                    "punch",
                    format!("[Leave] Take {} damage.", ev.damage),
                    false,
                ),
            ],
            E::Duplicator => vec![
                o("duplicate", "[Use] Duplicate a card.", self.deck.is_empty()),
                o("leave", "[Leave]", false),
            ],
            E::KnowingSkull => vec![
                o(
                    "potion",
                    format!("[Bones] {} HP. Gain a potion.", ev.costs[0]),
                    self.player.hp <= ev.costs[0],
                ),
                o(
                    "gold",
                    format!("[Bones] {} HP. Gain 90 gold.", ev.costs[1]),
                    self.player.hp <= ev.costs[1],
                ),
                o(
                    "card",
                    format!("[Bones] {} HP. Gain a colorless card.", ev.costs[2]),
                    self.player.hp <= ev.costs[2],
                ),
                o(
                    "leave",
                    format!("[Leave] Take {} damage.", 6),
                    self.player.hp <= 6,
                ),
            ],
            E::Nloth => vec![
                o(
                    "trade1",
                    format!(
                        "[Trade] {:?} → Nloth's Gift.",
                        self.relics.get(ev.bag[0] as usize).map(|r| r.kind)
                    ),
                    false,
                ),
                o(
                    "trade2",
                    format!(
                        "[Trade] {:?} → Nloth's Gift.",
                        self.relics.get(ev.bag[1] as usize).map(|r| r.kind)
                    ),
                    false,
                ),
                o("leave", "[Leave]", false),
            ],
            E::TheJoust => vec![
                o(
                    "against",
                    "[Against] Bet 50 gold for 100.",
                    self.player.gold < 50,
                ),
                o("for", "[For] Bet 50 gold for 250.", self.player.gold < 50),
                o("decline", "[Decline]", false),
            ],
        }
    }

    // ── 结算 ──────────────────────────────────────────────

    /// 选择事件选项（buttonEffect）。结果屏选项 = 确认离开。
    /// （GoldenIdol 屏位：0 入口 / 1 陷阱三选 / 2 结果，不适用通用 screen==1 约定）
    pub fn choose_event_option(&mut self, idx: usize) {
        // 先取选项（事件尚在 self.event 中，禁用态可读），再 take 结算
        let (done, is_result, opts) = match self.event.as_ref() {
            None => return,
            Some(ev) => (
                ev.done,
                Self::is_result_screen(ev.kind, ev.screen),
                self.options_for(ev),
            ),
        };
        if done || is_result {
            if let Some(ev) = self.event.as_mut() {
                ev.done = true;
            }
            return;
        }
        if let Some(opt) = opts.get(idx) {
            if opt.disabled {
                return;
            }
        } else {
            return;
        }
        let Some(mut ev) = self.event.take() else {
            return;
        };
        match ev.kind {
            EventKind::BigFish => {
                match idx {
                    0 => self.player.hp = (self.player.hp + ev.heal_amt).min(self.player.max_hp),
                    1 => {
                        self.player.max_hp += 5;
                        self.player.hp += 5;
                    }
                    _ => {
                        self.deck.push(CardInst::new("Regret"));
                        self.grant_random_relic();
                    }
                }
                ev.screen = 1;
            }
            EventKind::Cleric => match idx {
                0 => {
                    self.player.gold -= 35;
                    self.player.hp = (self.player.hp + ev.heal_amt).min(self.player.max_hp);
                    ev.screen = 1;
                }
                1 => {
                    self.player.gold -= ev.gold_loss;
                    ev.pending.push(PendingPick::Remove);
                    ev.screen = 1;
                }
                _ => ev.screen = 1,
            },
            EventKind::DeadAdventurer => {
                if idx == 0 {
                    if self.rng.misc.random_start_end(0, 99) < ev.chance {
                        // 遭遇：金币 (25-35)+30 与遗物入奖励，接战斗
                        let g = self.rng.misc.random_start_end(25, 35) + 30;
                        self.rewards.push(RewardItem::Gold(g));
                        if let Some(k) = self.roll_relic_kind() {
                            self.rewards.push(RewardItem::Relic(k));
                        }
                        let kinds = match ev.enemy {
                            0 => {
                                use MonsterKind as M;
                                &[M::Sentry, M::Sentry, M::Sentry] as &[MonsterKind]
                            }
                            1 => {
                                use MonsterKind as M;
                                &[M::GremlinNob] as &[MonsterKind]
                            }
                            _ => {
                                use MonsterKind as M;
                                &[M::Lagavulin] as &[MonsterKind]
                            }
                        };
                        self.start_event_combat(kinds);
                        ev.screen = 1;
                    } else {
                        // randomReward：袋顶 + chance +25
                        ev.counter += 1;
                        ev.chance += 25;
                        match ev.bag.remove(0) {
                            0 => self.player.gold += 30,
                            2 => {
                                self.grant_random_relic();
                            }
                            _ => {}
                        }
                        if ev.counter == 3 {
                            ev.screen = 1;
                        }
                    }
                } else {
                    ev.screen = 1;
                }
            }
            EventKind::GoldenIdol => {
                if ev.screen == 0 {
                    if idx == 0 {
                        self.relics.push(RelicInst::new(RelicKind::GoldenIdol));
                        ev.screen = 1;
                    } else {
                        ev.screen = 2;
                        ev.done = true;
                    }
                } else if ev.screen == 1 {
                    match idx {
                        0 => self.deck.push(CardInst::new("Injury")),
                        1 => self.lose_hp(ev.damage),
                        _ => {
                            self.player.max_hp -= ev.maxhp_loss;
                            self.player.hp = self.player.hp.min(self.player.max_hp);
                        }
                    }
                    ev.screen = 2;
                    ev.done = true;
                }
            }
            EventKind::GoldenWing => {
                if idx == 0 {
                    self.lose_hp(ev.damage);
                    let g = self.rng.misc.random_start_end(50, 80);
                    self.player.gold += g;
                    ev.screen = 1;
                } else {
                    ev.pending.push(PendingPick::GoldenWingRemove);
                    ev.screen = 1;
                }
            }
            EventKind::GoopPuddle => {
                if idx == 0 {
                    self.lose_hp(ev.damage);
                    self.player.gold += ev.gold;
                } else {
                    self.player.gold = (self.player.gold - ev.gold_loss).max(0);
                }
                ev.screen = 1;
            }
            EventKind::LiarsGame => {
                if idx == 0 {
                    self.deck.push(CardInst::new("Doubt"));
                    self.player.gold += ev.gold;
                }
                ev.screen = 1;
            }
            EventKind::LivingWall => {
                let pick = match idx {
                    0 => PendingPick::Remove,
                    1 => PendingPick::Transform,
                    _ => PendingPick::Upgrade,
                };
                let empty = match pick {
                    PendingPick::Upgrade => self.deck_upgradable().is_empty(),
                    _ => self.deck_purgeable().is_empty(),
                };
                if !empty {
                    ev.pending.push(pick); // 源码：无可选卡 → 什么都不发生
                }
                ev.screen = 1;
            }
            EventKind::Mushrooms => {
                if idx == 0 {
                    // ⚠️ 偏差：Mushroomer 未实现——战利直接发放，无战斗
                    let g = self.rng.misc.random_start_end(20, 30);
                    self.rewards.push(RewardItem::Gold(g));
                    self.relics.push(RelicInst::new(RelicKind::OddMushroom));
                } else {
                    self.player.hp = (self.player.hp + ev.heal_amt).min(self.player.max_hp);
                    self.deck.push(CardInst::new("Parasite"));
                }
                ev.screen = 1;
            }
            EventKind::ScrapOoze => {
                if idx == 0 {
                    self.lose_hp(ev.damage);
                    let roll = self.rng.misc.random_start_end(0, 99);
                    if roll >= 99 - ev.chance {
                        self.grant_random_relic();
                        ev.screen = 1;
                    } else {
                        ev.chance += 10;
                        ev.damage += 1;
                    }
                } else {
                    ev.screen = 1;
                }
            }
            EventKind::ShiningLight => {
                if idx == 0 {
                    self.lose_hp(ev.damage);
                    self.upgrade_two_random();
                }
                ev.screen = 1;
            }
            EventKind::GoldShrine => {
                match idx {
                    0 => self.player.gold += ev.gold,
                    1 => {
                        self.player.gold += 275;
                        self.deck.push(CardInst::new("Regret"));
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::Transmogrifier => {
                ev.pending.push(PendingPick::Transform);
                ev.screen = 1;
            }
            EventKind::PurificationShrine => {
                ev.pending.push(PendingPick::Remove);
                ev.screen = 1;
            }
            EventKind::UpgradeShrine => {
                ev.pending.push(PendingPick::Upgrade);
                ev.screen = 1;
            }
            EventKind::WheelOfChange => {
                // applyResult：miscRng.random(0,5)
                match self.rng.misc.random_start_end(0, 5) {
                    0 => self.player.gold += ev.gold,
                    1 => {
                        self.grant_random_relic();
                    }
                    2 => self.player.hp = self.player.max_hp,
                    3 => self.deck.push(CardInst::new("Decay")),
                    4 => {
                        if !self.deck_purgeable().is_empty() {
                            ev.pending.push(PendingPick::Remove);
                        }
                    }
                    _ => {
                        let pct = if self.ascension >= 15 { 0.15 } else { 0.10 };
                        let dmg = (self.player.max_hp as f32 * pct) as i32;
                        self.lose_hp(dmg);
                    }
                }
                ev.screen = 1;
            }
            EventKind::AccursedBlacksmith => {
                match idx {
                    0 => ev.pending.push(PendingPick::Upgrade),
                    1 => {
                        self.deck.push(CardInst::new("Pain"));
                        self.relics.push(RelicInst::new(RelicKind::WarpedTongs));
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::Bonfire => {
                ev.pending.push(PendingPick::BonfireOffer);
                ev.screen = 1;
            }
            EventKind::FaceTrader => {
                match idx {
                    0 => {
                        self.player.gold += ev.gold;
                        self.lose_hp(ev.damage);
                    }
                    1 => {
                        // ⚠️ 偏差：面具遗物未入池，以已实现池替代
                        self.grant_random_relic();
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::FountainOfCurseRemoval => {
                if idx == 0 {
                    self.deck
                        .retain(|c| c.def.card_type != "CURSE" || UNPURGEABLE.contains(&c.def.id));
                }
                ev.screen = 1;
            }
            EventKind::Lab => {
                if idx == 0 {
                    let n = if self.ascension >= 15 { 2 } else { 3 };
                    for _ in 0..n {
                        let kind = self.random_potion_uniform();
                        self.rewards.push(RewardItem::Potion(kind));
                    }
                }
                ev.screen = 1;
            }
            EventKind::WeMeetAgain => {
                // 选项序（event_options 同序）：药水（如有）/ 金币（如预计算>0）/
                // 卡牌（如有）/ 离开
                let mut slots: Vec<&'static str> = Vec::new();
                if self.potions.iter().any(|p| p.is_some()) {
                    slots.push("potion");
                }
                if ev.gold > 0 {
                    slots.push("gold");
                }
                if !self.deck.is_empty() {
                    slots.push("card");
                }
                slots.push("leave");
                match slots.get(idx).copied() {
                    Some("potion") => {
                        if let Some(slot) = self.potions.iter().position(|p| p.is_some()) {
                            self.potions[slot] = None;
                        }
                        self.grant_random_relic();
                    }
                    Some("gold") => {
                        self.player.gold = (self.player.gold - ev.gold).max(0);
                        self.grant_random_relic();
                    }
                    Some("card") => ev.pending.push(PendingPick::GiveCard),
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::WomanInBlue => {
                match idx {
                    0 => {
                        self.player.gold -= 20;
                        let k = self.random_potion_uniform();
                        self.rewards.push(RewardItem::Potion(k));
                    }
                    1 => {
                        self.player.gold -= 30;
                        for _ in 0..2 {
                            let k = self.random_potion_uniform();
                            self.rewards.push(RewardItem::Potion(k));
                        }
                    }
                    2 => {
                        self.player.gold -= 40;
                        for _ in 0..3 {
                            let k = self.random_potion_uniform();
                            self.rewards.push(RewardItem::Potion(k));
                        }
                    }
                    _ => {
                        if self.ascension >= 15 {
                            let dmg = ((self.player.max_hp as f32) * 0.05).ceil() as i32;
                            self.lose_hp(dmg);
                        }
                    }
                }
                ev.screen = 1;
            }
            // ── Act2 ──
            EventKind::Addict => {
                match idx {
                    0 => {
                        self.player.gold -= 85;
                        self.grant_random_relic();
                    }
                    1 => {
                        self.deck.push(CardInst::new("Shame"));
                        self.grant_random_relic();
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::BackToBasics => {
                if idx == 0 {
                    ev.pending.push(PendingPick::Remove);
                } else {
                    for c in self.deck.iter_mut() {
                        if c.def.id == "Strike_R" || c.def.id == "Defend_R" {
                            c.upgraded = true;
                        }
                    }
                }
                ev.screen = 1;
            }
            EventKind::Beggar => {
                if idx == 0 {
                    self.player.gold -= 75;
                    ev.pending.push(PendingPick::Remove);
                }
                ev.screen = 1;
            }
            EventKind::Colosseum => {
                if ev.screen == 0 {
                    // 第一波：无奖励（rewardAllowed=false）
                    use crate::state::MonsterKind as M;
                    ev.counter = 1;
                    self.start_event_combat(&[M::SlaverBlue, M::SlaverRed]);
                    ev.screen = 2; // 战斗中；胜利回调 → POST（screen 1）
                } else if ev.screen == 1 {
                    if idx == 0 {
                        use crate::state::MonsterKind as M;
                        ev.counter = 2;
                        self.rewards.push(RewardItem::Gold(100));
                        if let Some(k) = self.roll_relic_kind() {
                            self.rewards.push(RewardItem::Relic(k));
                        }
                        if let Some(k) = self.roll_relic_kind() {
                            self.rewards.push(RewardItem::Relic(k));
                        }
                        self.start_event_combat(&[M::Taskmaster, M::GremlinNob]);
                        ev.screen = 2;
                    } else {
                        ev.screen = 3;
                        ev.done = true;
                    }
                }
            }
            EventKind::DrugDealer => {
                match idx {
                    0 => {
                        if !self
                            .relics
                            .iter()
                            .any(|r| r.kind == RelicKind::MutagenicStrength)
                        {
                            self.relics
                                .push(RelicInst::new(RelicKind::MutagenicStrength));
                        } else {
                            self.grant_random_relic();
                        }
                    }
                    1 => {
                        ev.pending.push(PendingPick::Transform);
                        ev.pending.push(PendingPick::Transform);
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::ForgottenAltar => {
                let has_idol = self.relics.iter().any(|r| r.kind == RelicKind::GoldenIdol);
                match (has_idol, idx) {
                    (true, 0) => {
                        // 金神像 → 血神像（换件）
                        self.relics.retain(|r| r.kind != RelicKind::GoldenIdol);
                        self.relics.push(RelicInst::new(RelicKind::BloodyIdol));
                    }
                    (true, 1) | (false, 0) => {
                        self.player.max_hp += 5;
                        self.player.hp += 5;
                        self.lose_hp(ev.damage);
                    }
                    _ => {
                        self.deck.push(CardInst::new("Decay"));
                    }
                }
                ev.screen = 1;
            }
            EventKind::Ghosts => {
                if idx == 0 {
                    self.player.max_hp -= ev.maxhp_loss;
                    self.player.hp = self.player.hp.min(self.player.max_hp);
                    let n = if self.ascension >= 15 { 3 } else { 5 };
                    for _ in 0..n {
                        self.deck.push(CardInst::new("Ghostly"));
                    }
                }
                ev.screen = 1;
            }
            EventKind::MaskedBandits => {
                if idx == 0 {
                    self.player.gold = 0;
                } else {
                    // ⚠️ 偏差：BanditPointy/Leader/Bear 行为层未实现——
                    // 战利直发（金币 25-35 + 红面具），无战斗实体
                    let g = self.rng.misc.random_start_end(25, 35);
                    self.rewards.push(RewardItem::Gold(g));
                    self.relics.push(RelicInst::new(RelicKind::RedMask));
                }
                ev.screen = 1;
            }
            EventKind::Nest => {
                if idx == 0 {
                    self.player.gold += ev.gold;
                } else {
                    self.lose_hp(6);
                    self.deck.push(CardInst::new("RitualDagger"));
                }
                ev.screen = 1;
            }
            EventKind::TheLibrary => {
                if idx == 0 {
                    // 20 张唯一卡（rollRarity 3/37 + 暴雪偏移；去重重掷）
                    for _ in 0..20 {
                        loop {
                            let rarity = self.roll_rarity_bases(3, 37);
                            let card = self.pick_red_card(rarity);
                            if !ev.library.iter().any(|c| c.def.id == card.def.id) {
                                ev.library.push(card);
                                break;
                            }
                        }
                    }
                    ev.screen = 1; // 待 event_library_pick
                } else {
                    let heal = ((self.player.max_hp as f32)
                        * if self.ascension >= 15 { 0.2 } else { 0.33 }
                        + 0.5) as i32;
                    self.player.hp = (self.player.hp + heal).min(self.player.max_hp);
                    ev.screen = 2;
                    ev.done = true;
                }
            }
            EventKind::TheMausoleum => {
                if idx == 0 {
                    let cursed = if self.ascension >= 15 {
                        true
                    } else {
                        self.rng.misc.random_boolean()
                    };
                    if cursed {
                        self.deck.push(CardInst::new("Writhe"));
                    }
                    self.grant_random_relic();
                }
                ev.screen = 1;
            }
            EventKind::Vampires => {
                let has_vial = self.relics.iter().any(|r| r.kind == RelicKind::BloodVial);
                match (has_vial, idx) {
                    (_, 0) => {
                        self.player.max_hp -= ev.maxhp_loss;
                        self.player.hp = self.player.hp.min(self.player.max_hp);
                        self.replace_strikes_with_bites();
                    }
                    (true, 1) => {
                        self.relics.retain(|r| r.kind != RelicKind::BloodVial);
                        self.replace_strikes_with_bites();
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            // ── Act3 ──
            EventKind::Falling => {
                if ev.screen == 0 {
                    ev.screen = 1;
                } else {
                    let i = ev.bag[idx.min(2)] as usize;
                    if i < self.deck.len() {
                        self.deck.remove(i);
                    }
                    ev.screen = 2;
                    ev.done = true;
                }
            }
            EventKind::MindBloom => {
                match idx {
                    0 => {
                        // Boss 三选一（偏差：miscRng uniform 替代 JavaRandom 洗牌）
                        use crate::state::MonsterKind as M;
                        let boss = match self.rng.misc.random_start_end(0, 2) {
                            0 => &[M::Guardian] as &[crate::state::MonsterKind],
                            1 => &[M::Hexaghost] as &[crate::state::MonsterKind],
                            _ => &[M::SlimeBoss] as &[crate::state::MonsterKind],
                        };
                        self.rewards.push(RewardItem::Gold(ev.gold));
                        if let Some(k) = self.roll_relic_kind() {
                            self.rewards.push(RewardItem::Relic(k));
                        }
                        self.start_event_combat(boss);
                    }
                    1 => {
                        for c in self.deck.iter_mut() {
                            if !c.upgraded {
                                c.upgraded = true;
                            }
                        }
                        self.relics.push(RelicInst::new(RelicKind::MarkOfTheBloom));
                    }
                    _ => {
                        if self.floor % 50 <= 40 {
                            self.player.gold += 999;
                            self.deck.push(CardInst::new("Normality"));
                            self.deck.push(CardInst::new("Normality"));
                        } else {
                            self.player.hp = self.player.max_hp;
                            self.deck.push(CardInst::new("Doubt"));
                        }
                    }
                }
                if ev.screen == 0 {
                    ev.screen = 1;
                }
            }
            EventKind::MysteriousSphere => {
                if idx == 0 {
                    use crate::state::MonsterKind as M;
                    let g = self.rng.misc.random_start_end(45, 55);
                    self.rewards.push(RewardItem::Gold(g));
                    if let Some(k) = self.roll_relic_kind() {
                        self.rewards.push(RewardItem::Relic(k));
                    }
                    self.start_event_combat(&[M::OrbWalker, M::OrbWalker]);
                }
                ev.screen = 1;
            }
            EventKind::SensoryStone => {
                let (dmg, count) = match idx {
                    0 => (0, 1),
                    1 => (5, 2),
                    _ => (10, 3),
                };
                if dmg > 0 {
                    self.lose_hp(dmg);
                }
                // 每份记忆 = 3 选 1 无色卡（偏差：cardRng 直抽代替玩家选）
                for _ in 0..count {
                    let card = self.pick_colorless_reward();
                    self.deck.push(card);
                }
                ev.screen = 1;
            }
            EventKind::TombRedMask => {
                let has_mask = self.relics.iter().any(|r| r.kind == RelicKind::RedMask);
                match (has_mask, idx) {
                    (_, 0) => self.player.gold += 222,
                    (false, 1) => {
                        self.player.gold = 0;
                        self.relics.push(RelicInst::new(RelicKind::RedMask));
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::WindingHalls => {
                match idx {
                    0 => {
                        self.lose_hp(ev.damage);
                        self.deck.push(CardInst::new("Madness"));
                        self.deck.push(CardInst::new("Madness"));
                    }
                    1 => {
                        self.player.hp = (self.player.hp + ev.heal_amt).min(self.player.max_hp);
                        self.deck.push(CardInst::new("Writhe"));
                    }
                    _ => {
                        self.player.max_hp -= ev.maxhp_loss;
                        self.player.hp = self.player.hp.min(self.player.max_hp);
                    }
                }
                ev.screen = 1;
            }
            // ── 城市专属一次性 ──
            EventKind::Designer => {
                match idx {
                    0 => {
                        self.player.gold -= ev.gold;
                        if ev.chance == 1 {
                            ev.pending.push(PendingPick::Upgrade);
                        } else {
                            self.upgrade_two_random();
                        }
                    }
                    1 => {
                        self.player.gold -= ev.gold_loss;
                        if ev.counter == 1 {
                            ev.pending.push(PendingPick::Remove);
                        } else {
                            ev.pending.push(PendingPick::Transform);
                            ev.pending.push(PendingPick::Transform);
                        }
                    }
                    2 => {
                        self.player.gold -= ev.maxhp_loss;
                        ev.pending.push(PendingPick::Remove);
                        ev.pending.push(PendingPick::Upgrade);
                    }
                    _ => {
                        self.lose_hp(ev.damage);
                    }
                }
                ev.screen = 1;
            }
            EventKind::Duplicator => {
                if idx == 0 {
                    ev.pending.push(PendingPick::Duplicate);
                }
                ev.screen = 1;
            }
            EventKind::KnowingSkull => {
                match idx {
                    0 => {
                        self.lose_hp(ev.costs[0]);
                        ev.costs[0] += 1;
                        let k = self.random_potion_uniform();
                        if let Some(s) = self.potions.iter_mut().find(|p| p.is_none()) {
                            *s = Some(crate::potions::PotionInst::new(k));
                        }
                    }
                    1 => {
                        self.lose_hp(ev.costs[1]);
                        ev.costs[1] += 1;
                        self.player.gold += 90;
                    }
                    2 => {
                        self.lose_hp(ev.costs[2]);
                        ev.costs[2] += 1;
                        let card = self.pick_colorless_uncommon();
                        self.deck.push(card);
                    }
                    _ => {
                        self.lose_hp(6);
                        ev.screen = 1;
                        ev.done = true;
                    }
                }
                // 前三档可重复购买（留在主屏）；离开进结果屏
            }
            EventKind::Nloth => {
                match idx {
                    0 | 1 => {
                        let i = ev.bag[idx.min(1)] as usize;
                        if let Some(r) = self.relics.get(i) {
                            let kind = r.kind;
                            self.relics.retain(|r| r.kind != kind);
                            self.relics.push(RelicInst::new(RelicKind::NlothsGift));
                        }
                    }
                    _ => {}
                }
                ev.screen = 1;
            }
            EventKind::TheJoust => {
                if idx == 0 || idx == 1 {
                    self.player.gold -= 50;
                    let owner_wins = self.rng.misc.random_boolean_chance(0.3);
                    if idx == 0 && !owner_wins {
                        self.player.gold += 100;
                    } else if idx == 1 && owner_wins {
                        self.player.gold += 250;
                    }
                }
                ev.screen = 1;
            }
        }
        self.event = Some(ev);
    }

    /// 结果屏判定（多屏事件按 kind 自定义：GoldenIdol 2 / Colosseum 3 /
    /// Falling 2 / TheLibrary 2 / KnowingSkull 1；其余 screen≥1）
    fn is_result_screen(kind: EventKind, screen: u8) -> bool {
        use EventKind as E;
        match kind {
            E::GoldenIdol | E::Falling | E::TheLibrary => screen >= 2,
            E::Colosseum => screen >= 3,
            _ => screen >= 1,
        }
    }

    /// 事件内选牌结算（gridSelect 回调；pending 队列逐项消耗）
    pub fn event_pick_card(&mut self, deck_idx: usize) {
        let Some(mut ev) = self.event.take() else {
            return;
        };
        let Some(pick) = ev.pending.first().copied() else {
            self.event = Some(ev);
            return;
        };
        if deck_idx >= self.deck.len() {
            self.event = Some(ev);
            return;
        }
        let _ = ev.pending.remove(0);
        match pick {
            PendingPick::Remove => {
                self.deck.remove(deck_idx);
            }
            PendingPick::Transform => {
                self.deck.remove(deck_idx);
                let new_card = self.transform_random_card();
                self.deck.push(new_card);
            }
            PendingPick::Upgrade => {
                if let Some(c) = self.deck.get_mut(deck_idx) {
                    c.upgraded = true;
                }
            }
            PendingPick::GiveCard => {
                self.deck.remove(deck_idx);
                self.grant_random_relic();
            }
            PendingPick::BonfireOffer => {
                let rarity = self.deck[deck_idx].def.rarity;
                self.deck.remove(deck_idx);
                match rarity {
                    "CURSE" => self.relics.push(RelicInst::new(RelicKind::SpiritPoop)),
                    "BASIC" => {}
                    "COMMON" | "SPECIAL" => {
                        self.player.hp = (self.player.hp + 5).min(self.player.max_hp)
                    }
                    "UNCOMMON" => self.player.hp = self.player.max_hp,
                    _ => {
                        // RARE：回满 + Max HP +10
                        self.player.max_hp += 10;
                        self.player.hp = self.player.max_hp;
                    }
                }
            }
            PendingPick::GoldenWingRemove => {
                self.deck.remove(deck_idx);
                self.lose_hp(7);
            }
            PendingPick::Duplicate => {
                let copy = self.deck[deck_idx];
                self.deck.push(copy);
            }
        }
        self.event = Some(ev);
    }

    /// The Library：20 张候选中选一（协议层 grid 回调）
    pub fn event_library_pick(&mut self, lib_idx: usize) {
        let Some(mut ev) = self.event.take() else {
            return;
        };
        if ev.kind == EventKind::TheLibrary && ev.screen == 1 {
            if let Some(card) = ev.library.get(lib_idx) {
                let card = *card;
                self.deck.push(card);
                ev.screen = 2;
                ev.done = true;
            }
        }
        self.event = Some(ev);
    }

    // ── 事件公用原语 ──────────────────────────────────────

    /// returnRandomRelicTier + 已实现池发放（⚠️ tier 骰消耗 relicRng）
    pub fn grant_random_relic(&mut self) {
        if let Some(k) = self.roll_relic_kind() {
            self.relics.push(RelicInst::new(k));
        }
    }

    /// relicRng.random(99)：<50 COMMON / >85 RARE / 其余 UNCOMMON；
    /// 具体遗物从已实现池取未持有者（⚠️ 偏差：源码从预洗的 tier 池弹出）
    pub fn roll_relic_kind(&mut self) -> Option<RelicKind> {
        let _tier = self.rng.relic.random_start_end(0, 99);
        let owned: Vec<RelicKind> = self.relics.iter().map(|r| r.kind).collect();
        let pool: Vec<RelicKind> = RunState::IMPLEMENTED_RELICS
            .iter()
            .copied()
            .filter(|k| !owned.contains(k))
            .collect();
        if pool.is_empty() {
            return None;
        }
        let roll = self.rng.relic.random_range(pool.len() as i32 - 1).max(0) as usize;
        Some(pool[roll.min(pool.len() - 1)])
    }

    /// PotionHelper.getRandomPotion：potionRng 全池均匀（无稀有度筛）
    fn random_potion_uniform(&mut self) -> PotionKind {
        let pool = crate::potions::pool_for_class("Ironclad");
        let roll = self.rng.potion.random_range(pool.len() as i32 - 1).max(0) as usize;
        pool[roll.min(pool.len() - 1)]
    }

    /// transformCard：换成全色池随机卡（⚠️ 偏差：以 RED 池 + miscRng 替代
    /// returnTrulyRandomCardFromAvailable）
    fn transform_random_card(&mut self) -> CardInst {
        let pool: Vec<&'static crate::effects::CardDef> = CARDS
            .iter()
            .filter(|d| d.color == "RED" && d.status != "no_use")
            .collect();
        let roll = self.rng.misc.random_range(pool.len() as i32 - 1).max(0) as usize;
        CardInst {
            def: pool[roll.min(pool.len() - 1)],
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        }
    }

    /// ShiningLight：升级 2 张随机可升级卡（⚠️ 偏差：miscRng 无放回抽取替代
    /// JavaRandom 种子洗牌）
    fn upgrade_two_random(&mut self) {
        let mut idxs = self.deck_upgradable();
        let mut picks = Vec::new();
        for _ in 0..2 {
            if idxs.is_empty() {
                break;
            }
            let j = (self.rng.misc.random_range(idxs.len() as i32 - 1).max(0) as usize)
                .min(idxs.len() - 1);
            picks.push(idxs.remove(j));
        }
        for i in picks {
            self.deck[i].upgraded = true;
        }
    }

    /// 事件伤害（HP_LOSS 语义：可致死）
    fn lose_hp(&mut self, amount: i32) {
        self.player.hp -= amount;
        if self.player.hp <= 0 {
            self.player.hp = 0;
            self.over = true;
        }
    }

    /// 事件战斗：以怪物房同款种子派生 + 事件标记（胜利后事件收尾）
    fn start_event_combat(&mut self, kinds: &'static [MonsterKind]) {
        self.event_combat = true;
        let relics: Vec<_> = self.relics.clone();
        self.combat = Some(Combat::new_run_battle(
            self.seed ^ (self.floor as u64 + 1) << 32,
            self.ascension,
            kinds,
            relics,
            self.deck.clone(),
            self.potions.clone(),
            self.player.hp,
            self.player.max_hp,
            self.player.gold,
        ));
    }

    /// 事件战斗结束收尾（settle_combat 回调）：Colosseum 第一波后进波间屏，
    /// 其余（Dead Adventurer / MindBloom / MysteriousSphere / 第二波）直接收尾
    pub(crate) fn event_after_combat(&mut self, won: bool) {
        let Some(ev) = self.event.as_mut() else {
            return;
        };
        if !won {
            return; // 失败：局外层处理死亡，事件不再推进
        }
        match ev.kind {
            EventKind::Colosseum if ev.counter == 1 => {
                ev.screen = 1; // POST：[Fight Nobs] / [Flee]
            }
            _ => {
                ev.screen = ev.screen.max(1);
                ev.done = true;
            }
        }
    }

    /// rollRarity 通用基底（run.rs 奖励 3/37、商店 9/37、图书馆 3/37 共用）
    pub(crate) fn roll_rarity_bases(&mut self, rare_base: i32, unc_base: i32) -> &'static str {
        let roll = self.rng.card.random_range(99) + self.card_blizz;
        if roll < rare_base {
            self.card_blizz = 5;
            "RARE"
        } else if roll < rare_base + unc_base {
            "UNCOMMON"
        } else {
            self.card_blizz = (self.card_blizz - 1).max(-40);
            "COMMON"
        }
    }

    /// getCard：指定稀有度红卡均匀抽取（cardRng；无类型筛）
    fn pick_red_card(&mut self, rarity: &str) -> CardInst {
        let pool: Vec<&'static crate::effects::CardDef> = CARDS
            .iter()
            .filter(|d| d.color == "RED" && d.rarity == rarity && d.status != "no_use")
            .collect();
        if pool.is_empty() {
            return CardInst::new("Strike_R");
        }
        let roll = self.rng.card.random_range(pool.len() as i32 - 1).max(0) as usize;
        CardInst {
            def: pool[roll.min(pool.len() - 1)],
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        }
    }

    /// getColorlessRewardCards 单张：rollRareOrUncommon(0.12) → 无色池均匀
    fn pick_colorless_reward(&mut self) -> CardInst {
        let rarity = if self.rng.card.random_boolean_chance(0.12) {
            "RARE"
        } else {
            "UNCOMMON"
        };
        self.pick_colorless_of(rarity)
    }

    /// KnowingSkull：随机无色 UNCOMMON
    fn pick_colorless_uncommon(&mut self) -> CardInst {
        self.pick_colorless_of("UNCOMMON")
    }

    fn pick_colorless_of(&mut self, rarity: &str) -> CardInst {
        let pool: Vec<&'static crate::effects::CardDef> = CARDS
            .iter()
            .filter(|d| d.color == "COLORLESS" && d.rarity == rarity && d.status != "no_use")
            .collect();
        if pool.is_empty() {
            return CardInst::new("Strike_R");
        }
        let roll = self.rng.card.random_range(pool.len() as i32 - 1).max(0) as usize;
        CardInst {
            def: pool[roll.min(pool.len() - 1)],
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        }
    }

    /// Vampires：移除全部初始打击，加 5 张噬咬
    fn replace_strikes_with_bites(&mut self) {
        self.deck.retain(|c| c.def.id != "Strike_R");
        for _ in 0..5 {
            self.deck.push(CardInst::new("Bite"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state(kind: EventKind) -> EventState {
        EventState {
            kind,
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
            pending: Vec::new(),
            library: Vec::new(),
            costs: [0; 3],
        }
    }

    /// 直接构造指定事件（跳过池抽取；构造期 rng 照常消耗）
    fn with_event(seed: u64, ascension: i32, kind: EventKind) -> RunState {
        let mut run = RunState::new(seed, ascension);
        run.floor = 10; // Dead Adventurer / Mushrooms 门槛（floor > 6）
        run.event = Some(run.construct_event(base_state(kind)));
        run
    }

    /// 池初始化：Act1 11 / Act2 12（CursedTome 未入池）/ Act3 6 +
    /// 神龛 5 + 一次性 12（城市专属 5 只本批接入）
    #[test]
    fn pools_initialized() {
        let run = RunState::new(3, 0);
        assert_eq!(run.event_pool.len(), 11, "Act1 事件全量");
        assert_eq!(run.shrine_pool.len(), 5, "Match and Keep! 未入池");
        assert_eq!(run.special_pool.len(), 12);
        let mut run2 = RunState::new(3, 0);
        run2.act = 2;
        run2.init_event_pools();
        assert_eq!(run2.event_pool.len(), 12, "Act2（CursedTome 偏差排除）");
        let mut run3 = RunState::new(3, 0);
        run3.act = 3;
        run3.init_event_pools();
        assert_eq!(run3.event_pool.len(), 6, "Act3 全量");
    }

    /// 事件抽取不重复：Act1 池 11 只各被抽到一次（floor=10 金币 99 全部可选；
    /// 神龛分支的抽取不计入事件池）
    #[test]
    fn events_draw_without_repeat() {
        let mut run = RunState::new(2024, 0);
        run.floor = 10;
        let act1 = act_event_pool(1);
        let mut seen: Vec<EventKind> = Vec::new();
        for _ in 0..60 {
            run.event = None;
            run.enter_event();
            if let Some(ev) = &run.event {
                if act1.contains(&ev.kind) {
                    assert!(!seen.contains(&ev.kind), "事件 {:?} 重复", ev.kind);
                    seen.push(ev.kind);
                }
            }
            if run.event_pool.is_empty() {
                break;
            }
        }
        assert_eq!(run.event_pool.len(), 0, "Act1 池耗尽");
        assert_eq!(seen.len(), 11, "11 只各出现一次");
    }

    /// Big Fish：香蕉回 1/3 最大生命 / 甜甜圈 +5 / 盒子 Regret+遗物
    #[test]
    fn big_fish_effects() {
        // 香蕉
        let mut run = with_event(1, 0, EventKind::BigFish);
        run.player.hp = 40;
        run.choose_event_option(0);
        assert_eq!(run.player.hp, 40 + 80 / 3, "maxHP 80 → 回 26");
        assert!(run.event.as_ref().unwrap().screen == 1);
        // 甜甜圈
        let mut run = with_event(1, 0, EventKind::BigFish);
        run.choose_event_option(1);
        assert_eq!(run.player.max_hp, 85);
        // 盒子
        let mut run = with_event(1, 0, EventKind::BigFish);
        let relics = run.relics.len();
        run.choose_event_option(2);
        assert!(run.deck.iter().any(|c| c.def.id == "Regret"), "Regret 入组");
        assert_eq!(run.relics.len(), relics + 1, "遗物 +1");
    }

    /// Golden Idol：拿神像 → 三选（伤 25% / 失忆 Injury / -8% MaxHP）
    #[test]
    fn golden_idol_flow() {
        let mut run = with_event(2, 0, EventKind::GoldenIdol);
        run.choose_event_option(0);
        assert!(run.relics.iter().any(|r| r.kind == RelicKind::GoldenIdol));
        assert_eq!(run.event.as_ref().unwrap().screen, 1, "陷阱屏");
        // 选择承受伤害：80×25% = 20
        let hp = run.player.hp;
        run.choose_event_option(1);
        assert_eq!(run.player.hp, hp - 20);
        assert!(run.event.as_ref().unwrap().done);
        // 换一种走法：-8% MaxHP = 6（80×0.08 → 6）
        let mut run = with_event(2, 0, EventKind::GoldenIdol);
        run.choose_event_option(0);
        run.choose_event_option(2);
        assert_eq!(run.player.max_hp, 80 - 6);
    }

    /// World of Goop：捡金币受 11 伤 +75 金 / 离开失 20-50 金
    #[test]
    fn goop_puddle() {
        let mut run = with_event(3, 0, EventKind::GoopPuddle);
        let loss = run.event.as_ref().unwrap().gold_loss;
        assert!((20..=50).contains(&loss), "金币损失区间 {loss}");
        run.choose_event_option(0);
        assert_eq!(run.player.hp, 80 - 11);
        assert_eq!(run.player.gold, 99 + 75);
        let mut run = with_event(3, 0, EventKind::GoopPuddle);
        let loss = run.event.as_ref().unwrap().gold_loss;
        run.choose_event_option(1);
        assert_eq!(run.player.gold, 99 - loss);
    }

    /// Golden Shrine：祈祷 +100 / 亵渎 +275 & Regret
    #[test]
    fn gold_shrine() {
        let mut run = with_event(4, 0, EventKind::GoldShrine);
        run.choose_event_option(0);
        assert_eq!(run.player.gold, 199);
        let mut run = with_event(4, 0, EventKind::GoldShrine);
        run.choose_event_option(1);
        assert_eq!(run.player.gold, 99 + 275);
        assert!(run.deck.iter().any(|c| c.def.id == "Regret"));
    }

    /// Wheel of Change：找到「金币」结果种子 → +100（Act1）
    #[test]
    fn wheel_gold_outcome() {
        for seed in 0..64u64 {
            let mut run = with_event(seed, 0, EventKind::WheelOfChange);
            run.choose_event_option(0);
            if run.player.gold == 199 && run.relics.len() == 1 && run.player.hp == 80 {
                assert_eq!(run.deck.len(), 10, "无诅咒无删卡");
                assert!(run.event.as_ref().unwrap().screen == 1);
                return;
            }
        }
        panic!("64 个种子内未命中金币结果（概率异常）");
    }

    /// The Cleric：净化 50 金删一卡（A15+ 75）
    #[test]
    fn cleric_purify() {
        let mut run = with_event(5, 0, EventKind::Cleric);
        run.choose_event_option(1);
        assert_eq!(run.player.gold, 99 - 50);
        let len = run.deck.len();
        run.event_pick_card(0);
        assert_eq!(run.deck.len(), len - 1, "删卡");
        // A15 价 75
        let mut run = with_event(5, 15, EventKind::Cleric);
        run.choose_event_option(1);
        assert_eq!(run.player.gold, 99 - 75);
    }

    /// Shining Light：受 round(80×0.2)=16 伤 + 升级 2 张随机卡
    #[test]
    fn shining_light() {
        let mut run = with_event(6, 0, EventKind::ShiningLight);
        run.choose_event_option(0);
        assert_eq!(run.player.hp, 80 - 16);
        assert_eq!(
            run.deck.iter().filter(|c| c.upgraded).count(),
            2,
            "升级 2 张"
        );
        // A15：round(80×0.3)=24，且 A6+ 起始生命 72
        let mut run = with_event(6, 15, EventKind::ShiningLight);
        run.choose_event_option(0);
        assert_eq!(run.player.hp, 72 - 24);
    }

    /// The Woman in Blue：30 金买 2 药水（进奖励列表）
    #[test]
    fn woman_in_blue() {
        let mut run = with_event(7, 0, EventKind::WomanInBlue);
        run.choose_event_option(1);
        assert_eq!(run.player.gold, 99 - 30);
        assert_eq!(
            run.rewards
                .iter()
                .filter(|r| matches!(r, RewardItem::Potion(_)))
                .count(),
            2
        );
    }

    /// WeMeetAgain：送药水换遗物
    #[test]
    fn we_meet_again_potion() {
        let mut run = with_event(8, 0, EventKind::WeMeetAgain);
        run.potions[0] = Some(crate::potions::PotionInst::new(
            crate::potions::PotionKind::Fire,
        ));
        let gold = run.player.gold;
        let relics = run.relics.len();
        run.choose_event_option(0); // 首个选项 = 送药水
        assert!(run.potions.iter().all(|p| p.is_none()), "药水被取走");
        assert_eq!(run.player.gold, gold, "送药水不动金币");
        assert_eq!(run.relics.len(), relics + 1, "遗物 +1");
    }

    /// Living Wall：成长 = 升级所选卡
    #[test]
    fn living_wall_grow() {
        let mut run = with_event(9, 0, EventKind::LivingWall);
        run.choose_event_option(2);
        assert_eq!(
            run.event.as_ref().unwrap().pending,
            vec![PendingPick::Upgrade]
        );
        run.event_pick_card(3);
        assert!(run.deck[3].upgraded);
    }

    /// Dead Adventurer：找到战斗种子（遭遇精英 + 金币/遗物预置奖励）
    #[test]
    fn dead_adventurer_fight() {
        for seed in 0..64u64 {
            let mut run = with_event(seed, 0, EventKind::DeadAdventurer);
            run.choose_event_option(0);
            if run.combat.is_some() {
                assert!(run.event_combat, "事件战斗标记");
                assert!(
                    run.rewards.iter().any(|r| matches!(r, RewardItem::Gold(_)))
                        && run
                            .rewards
                            .iter()
                            .any(|r| matches!(r, RewardItem::Relic(_))),
                    "战斗奖励预置（金币 55-65 + 遗物）"
                );
                let g: Vec<i32> = run
                    .rewards
                    .iter()
                    .filter_map(|r| match r {
                        RewardItem::Gold(g) => Some(*g),
                        _ => None,
                    })
                    .collect();
                assert_eq!(g.len(), 1);
                assert!(
                    (55..=65).contains(&g[0]),
                    "战利金币 = 25-35 + 30，实际 {}",
                    g[0]
                );
                return;
            }
        }
        panic!("64 个种子未触发遭遇（25% 起步概率异常）");
    }

    /// Dead Adventurer：搜索三次必结束（袋只有 3 项；chance=0 强制不遇敌）
    #[test]
    fn dead_adventurer_three_searches_end() {
        let mut run = with_event(0, 0, EventKind::DeadAdventurer);
        let mut searches = 0;
        loop {
            run.event.as_mut().unwrap().chance = 0; // roll < 0 不可能 → 必走奖励路径
            run.choose_event_option(0);
            searches += 1;
            assert!(searches <= 3, "袋只有 3 项，不应当超过 3 次搜索");
            if run.event.as_ref().unwrap().screen == 1 {
                assert_eq!(searches, 3, "3 次搜完");
                return;
            }
        }
    }

    /// Bonfire：献祭稀有卡 → 回满 + MaxHP +10
    #[test]
    fn bonfire_rare_sacrifice() {
        let mut run = with_event(10, 0, EventKind::Bonfire);
        // 注入一张稀有红卡
        let rare = CARDS
            .iter()
            .find(|d| d.color == "RED" && d.rarity == "RARE" && d.status != "no_use")
            .expect("红稀有卡存在");
        run.deck.push(CardInst {
            def: rare,
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        });
        run.player.hp = 10;
        run.choose_event_option(0);
        run.event_pick_card(run.deck.len() - 1);
        assert_eq!(run.player.max_hp, 90, "MaxHP +10");
        assert_eq!(run.player.hp, 90, "回满");
    }

    /// Fountain of Cleansing：移除全部可移除诅咒（AscendersBane 保留）
    #[test]
    fn fountain_removes_curses() {
        let mut run = with_event(11, 0, EventKind::FountainOfCurseRemoval);
        run.deck.push(CardInst::new("Regret"));
        run.deck.push(CardInst::new("AscendersBane"));
        run.choose_event_option(0);
        assert!(
            !run.deck.iter().any(|c| c.def.id == "Regret"),
            "Regret 移除"
        );
        assert!(
            run.deck.iter().any(|c| c.def.id == "AscendersBane"),
            "飞升者诅咒保留"
        );
    }

    /// Accursed Blacksmith：翻查 = Pain + WarpedTongs
    #[test]
    fn blacksmith_rummage() {
        let mut run = with_event(12, 0, EventKind::AccursedBlacksmith);
        run.choose_event_option(1);
        assert!(run.deck.iter().any(|c| c.def.id == "Pain"));
        assert!(run.relics.iter().any(|r| r.kind == RelicKind::WarpedTongs));
    }

    /// Mushrooms：逃走 = 回 20 + Parasite；开战 = 金币+遗物（偏差：无战斗实体）
    #[test]
    fn mushrooms() {
        let mut run = with_event(13, 0, EventKind::Mushrooms);
        run.player.hp = 40;
        run.choose_event_option(1);
        assert_eq!(run.player.hp, 40 + 20, "80×25% = 20");
        assert!(run.deck.iter().any(|c| c.def.id == "Parasite"));
        let mut run = with_event(13, 0, EventKind::Mushrooms);
        run.choose_event_option(0);
        assert!(run.relics.iter().any(|r| r.kind == RelicKind::OddMushroom));
        assert!(run.rewards.iter().any(|r| matches!(r, RewardItem::Gold(_))));
    }

    /// Scrap Ooze：到达时伤害 3（A15 5），失败 +10% 概率 +1 伤
    #[test]
    fn scrap_ooze_ramp() {
        let run = with_event(14, 0, EventKind::ScrapOoze);
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.damage, 3);
        assert_eq!(ev.chance, 25);
        let run = with_event(14, 15, EventKind::ScrapOoze);
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.damage, 5);
    }

    /// 事件选项禁用：Cleric 金币不足时治疗/净化禁用
    #[test]
    fn cleric_disabled_when_poor() {
        let mut run = with_event(15, 0, EventKind::Cleric);
        run.player.gold = 10;
        let opts = run.event_options();
        assert!(opts[0].disabled, "35 金不够 → 治疗禁用");
        assert!(opts[1].disabled, "50 金不够 → 净化禁用");
        // 禁用选项选择无效果
        run.choose_event_option(0);
        assert_eq!(run.player.gold, 10);
        assert_eq!(run.player.hp, 80);
    }
    // ── Act2/3 批次 ──────────────────────────────────────

    /// Addict：付 85 金得遗物 / 偷：Shame + 遗物
    #[test]
    fn addict_effects() {
        let mut run = with_event(21, 0, EventKind::Addict);
        run.choose_event_option(0);
        assert_eq!(run.player.gold, 99 - 85);
        assert_eq!(run.relics.len(), 2, "遗物 +1");
        let mut run = with_event(21, 0, EventKind::Addict);
        run.choose_event_option(1);
        assert_eq!(run.player.gold, 99, "偷不动钱");
        assert!(run.deck.iter().any(|c| c.def.id == "Shame"));
        assert_eq!(run.relics.len(), 2);
    }

    /// Back to Basics：升级全部打击/防御（5 打击 4 防御）
    #[test]
    fn back_to_basics_upgrade() {
        let mut run = with_event(22, 0, EventKind::BackToBasics);
        run.choose_event_option(1);
        let upgraded = run
            .deck
            .iter()
            .filter(|c| c.upgraded && (c.def.id == "Strike_R" || c.def.id == "Defend_R"))
            .count();
        assert_eq!(upgraded, 9, "5 打击 + 4 防御");
    }

    /// Beggar：付 75 删一卡
    #[test]
    fn beggar_purge() {
        let mut run = with_event(23, 0, EventKind::Beggar);
        run.choose_event_option(0);
        assert_eq!(run.player.gold, 99 - 75);
        let len = run.deck.len();
        run.event_pick_card(0);
        assert_eq!(run.deck.len(), len - 1);
    }

    /// Colosseum：第一波无奖励 → 波间屏 → 第二波预置 100 金 + 2 遗物
    #[test]
    fn colosseum_waves() {
        let mut run = with_event(24, 0, EventKind::Colosseum);
        run.choose_event_option(0); // 第一波
        assert!(run.combat.is_some(), "第一波开打");
        assert!(
            run.rewards.is_empty(),
            "第一波无奖励（rewardAllowed=false）"
        );
        // 模拟胜利
        if let Some(c) = run.combat.as_mut() {
            for m in c.monsters.iter_mut() {
                m.creature.hp = 0;
            }
            c.over = true;
            c.won = true;
        }
        run.settle_combat(0);
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.screen, 1, "波间屏（POST）");
        assert!(!ev.done);
        // 逃跑
        run.choose_event_option(1);
        assert!(run.event.as_ref().unwrap().done);
        // 另一局：打第二波（先结算第一波 → 波间屏 → 选择打 Nobs）
        let mut run = with_event(24, 0, EventKind::Colosseum);
        run.choose_event_option(0);
        if let Some(c) = run.combat.as_mut() {
            for m in c.monsters.iter_mut() {
                m.creature.hp = 0;
            }
            c.over = true;
            c.won = true;
        }
        run.settle_combat(0);
        assert_eq!(run.event.as_ref().unwrap().screen, 1, "波间屏");
        run.choose_event_option(0);
        assert!(run.combat.is_some(), "第二波开打");
        assert!(run
            .rewards
            .iter()
            .any(|r| matches!(r, RewardItem::Gold(100))));
        assert_eq!(
            run.rewards
                .iter()
                .filter(|r| matches!(r, RewardItem::Relic(_)))
                .count(),
            2,
            "RARE + UNCOMMON 两件"
        );
    }

    /// DrugDealer：注变体 / 试药：两张变化
    #[test]
    fn drug_dealer() {
        let mut run = with_event(25, 0, EventKind::DrugDealer);
        run.choose_event_option(0);
        assert!(run
            .relics
            .iter()
            .any(|r| r.kind == RelicKind::MutagenicStrength));
        let mut run = with_event(25, 0, EventKind::DrugDealer);
        run.choose_event_option(1);
        assert_eq!(run.event.as_ref().unwrap().pending.len(), 2, "两张待变化");
    }

    /// ForgottenAltar：流血 = MaxHP+5 + 25% 伤；有金神像时可换血神像
    #[test]
    fn forgotten_altar() {
        let mut run = with_event(26, 0, EventKind::ForgottenAltar);
        // 无神像：选项 = [流血][砸碎]
        run.choose_event_option(0);
        assert_eq!(run.player.max_hp, 85, "MaxHP +5");
        assert_eq!(run.player.hp, 80 + 5 - 20, "80×25% = 20 伤");
        // 有神像：[供奉][流血][砸碎]
        let mut run = with_event(26, 0, EventKind::ForgottenAltar);
        run.relics.push(RelicInst::new(RelicKind::GoldenIdol));
        run.choose_event_option(0);
        assert!(run.relics.iter().any(|r| r.kind == RelicKind::BloodyIdol));
        assert!(!run.relics.iter().any(|r| r.kind == RelicKind::GoldenIdol));
    }

    /// Ghosts：失 ceil(50%) MaxHP 得 5 幽影（A15 得 3）
    #[test]
    fn ghosts_apparitions() {
        let mut run = with_event(27, 0, EventKind::Ghosts);
        run.choose_event_option(0);
        assert_eq!(run.player.max_hp, 80 - 40, "ceil(80×0.5)=40");
        assert_eq!(run.deck.iter().filter(|c| c.def.id == "Ghostly").count(), 5);
        let mut run = with_event(27, 15, EventKind::Ghosts);
        run.choose_event_option(0);
        assert_eq!(run.player.max_hp, 80 - 40);
        assert_eq!(
            run.deck.iter().filter(|c| c.def.id == "Ghostly").count(),
            3,
            "A15 只得 3 张"
        );
    }

    /// Nest：偷 99 金 / 入教 6 伤 + 仪式匕首
    #[test]
    fn nest_options() {
        let mut run = with_event(28, 0, EventKind::Nest);
        run.choose_event_option(0);
        assert_eq!(run.player.gold, 99 + 99);
        let mut run = with_event(28, 0, EventKind::Nest);
        run.choose_event_option(1);
        assert_eq!(run.player.hp, 80 - 6);
        assert!(run.deck.iter().any(|c| c.def.id == "RitualDagger"));
    }

    /// The Library：读到 20 张唯一卡后选一
    #[test]
    fn the_library() {
        let mut run = with_event(29, 0, EventKind::TheLibrary);
        run.choose_event_option(0);
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.library.len(), 20);
        let ids: Vec<&str> = ev.library.iter().map(|c| c.def.id).collect();
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(ids.len(), uniq.len(), "无重复");
        let len = run.deck.len();
        run.event_library_pick(3);
        assert_eq!(run.deck.len(), len + 1, "选卡入组");
        assert!(run.event.as_ref().unwrap().done);
    }

    /// The Mausoleum：50% 诅咒 + 必得遗物（A15 恒诅咒）
    #[test]
    fn mausoleum_a15_cursed() {
        let mut run = with_event(30, 15, EventKind::TheMausoleum);
        run.choose_event_option(0);
        assert!(run.deck.iter().any(|c| c.def.id == "Writhe"), "A15 恒诅咒");
        assert_eq!(run.relics.len(), 2, "遗物 +1");
    }

    /// Vampires：失 ceil(30%) MaxHP，打击全换成 5 噬咬；血瓶路径免损
    #[test]
    fn vampires() {
        let mut run = with_event(31, 0, EventKind::Vampires);
        run.choose_event_option(0);
        assert_eq!(run.player.max_hp, 80 - 24, "ceil(80×0.3)=24");
        assert!(!run.deck.iter().any(|c| c.def.id == "Strike_R"));
        assert_eq!(run.deck.iter().filter(|c| c.def.id == "Bite").count(), 5);
        let mut run = with_event(31, 0, EventKind::Vampires);
        run.relics.push(RelicInst::new(RelicKind::BloodVial));
        run.choose_event_option(1); // 血瓶路径
        assert_eq!(run.player.max_hp, 80, "不失 MaxHP");
        assert!(!run.relics.iter().any(|r| r.kind == RelicKind::BloodVial));
        assert_eq!(run.deck.iter().filter(|c| c.def.id == "Bite").count(), 5);
    }

    /// Falling：预选三张（技/能/攻各一），选择即删
    #[test]
    fn falling_removes_preselected() {
        let mut run = with_event(32, 0, EventKind::Falling);
        run.choose_event_option(0); // 入选择屏
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.screen, 1);
        let bag = ev.bag.clone();
        let target = run.deck[bag[2] as usize].def.id; // 攻击卡
        let before = run.deck.iter().filter(|c| c.def.id == target).count();
        let len = run.deck.len();
        run.choose_event_option(2);
        assert_eq!(run.deck.len(), len - 1);
        assert_eq!(
            run.deck.iter().filter(|c| c.def.id == target).count(),
            before - 1,
            "预选卡被移除"
        );
    }

    /// MindBloom：升级全卡 + 绽放印记；金币分支 999
    #[test]
    fn mind_bloom() {
        let mut run = with_event(33, 0, EventKind::MindBloom);
        run.floor = 10; // % 50 = 10 ≤ 40 → 金币分支
        run.choose_event_option(1);
        assert!(run.deck.iter().all(|c| c.upgraded), "全部升级");
        assert!(run
            .relics
            .iter()
            .any(|r| r.kind == RelicKind::MarkOfTheBloom));
        let mut run = with_event(33, 0, EventKind::MindBloom);
        run.floor = 10;
        run.choose_event_option(2);
        assert_eq!(run.player.gold, 99 + 999);
        assert_eq!(
            run.deck.iter().filter(|c| c.def.id == "Normality").count(),
            2
        );
    }

    /// MysteriousSphere：金币 45-55 + 遗物 + 双奥术行者
    #[test]
    fn mysterious_sphere_combat() {
        let mut run = with_event(34, 0, EventKind::MysteriousSphere);
        run.choose_event_option(0);
        assert!(run.combat.is_some());
        assert_eq!(run.combat.as_ref().unwrap().monsters.len(), 2);
        let g: Vec<i32> = run
            .rewards
            .iter()
            .filter_map(|r| match r {
                RewardItem::Gold(g) => Some(*g),
                _ => None,
            })
            .collect();
        assert_eq!(g.len(), 1);
        assert!((45..=55).contains(&g[0]));
    }

    /// SensoryStone：三档 = 0/5/10 伤 + 1/2/3 张无色卡
    #[test]
    fn sensory_stone_tiers() {
        let mut run = with_event(35, 0, EventKind::SensoryStone);
        let deck0 = run.deck.len();
        run.choose_event_option(1);
        assert_eq!(run.player.hp, 80 - 5);
        assert_eq!(run.deck.len(), deck0 + 2, "两张无色卡");
    }

    /// TombRedMask：戴面具取 222 金；无面具付光金币得红面具
    #[test]
    fn tomb_red_mask() {
        let mut run = with_event(36, 0, EventKind::TombRedMask);
        run.relics.push(RelicInst::new(RelicKind::RedMask));
        run.choose_event_option(0);
        assert_eq!(run.player.gold, 99 + 222);
        let mut run = with_event(36, 0, EventKind::TombRedMask);
        run.choose_event_option(1); // [取金(禁用)] [付光金币] [离开]
        assert_eq!(run.player.gold, 0);
        assert!(run.relics.iter().any(|r| r.kind == RelicKind::RedMask));
    }

    /// WindingHalls：拥抱疯狂 = 12.5% 伤 + 2 谵妄；开门 = 回 25% + 挣扎
    #[test]
    fn winding_halls() {
        let mut run = with_event(37, 0, EventKind::WindingHalls);
        run.choose_event_option(0);
        assert_eq!(run.player.hp, 80 - 10, "round(80×0.125)=10");
        assert_eq!(run.deck.iter().filter(|c| c.def.id == "Madness").count(), 2);
        let mut run = with_event(37, 0, EventKind::WindingHalls);
        run.player.hp = 30;
        run.choose_event_option(1);
        assert_eq!(run.player.hp, 30 + 20, "round(80×0.25)=20");
        assert!(run.deck.iter().any(|c| c.def.id == "Writhe"));
        let mut run = with_event(37, 0, EventKind::WindingHalls);
        run.choose_event_option(2);
        assert_eq!(run.player.max_hp, 80 - 4, "round(80×0.05)=4");
    }

    /// Designer：价目 40/60/90（A15 50/75/110）；全套 = 删 + 升两步
    #[test]
    fn designer_full_service() {
        let mut run = with_event(38, 0, EventKind::Designer);
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.gold, 40, "A0 调整价");
        assert_eq!(ev.gold_loss, 60, "A0 清洁价");
        assert_eq!(ev.maxhp_loss, 90, "A0 全套价");
        assert_eq!(ev.damage, 3, "A0 离场伤");
        run.choose_event_option(2); // 全套
        assert_eq!(run.player.gold, 99 - 90);
        assert_eq!(
            run.event.as_ref().unwrap().pending,
            vec![PendingPick::Remove, PendingPick::Upgrade]
        );
        let len = run.deck.len();
        run.event_pick_card(0); // 删
        assert_eq!(run.deck.len(), len - 1);
        run.event_pick_card(1); // 升
        assert!(run.deck[1].upgraded);
        assert!(run.event.as_ref().unwrap().pending.is_empty());
    }

    /// Duplicator：复制所选卡
    #[test]
    fn duplicator() {
        let mut run = with_event(39, 0, EventKind::Duplicator);
        let len = run.deck.len();
        run.choose_event_option(0);
        run.event_pick_card(2);
        assert_eq!(run.deck.len(), len + 1, "复制入组");
    }

    /// KnowingSkull：6 HP 一档，价 +1，可重复；离开也扣 6
    #[test]
    fn knowing_skull() {
        let mut run = with_event(40, 0, EventKind::KnowingSkull);
        run.choose_event_option(1); // 金币档
        assert_eq!(run.player.hp, 80 - 6);
        assert_eq!(run.player.gold, 99 + 90);
        let ev = run.event.as_ref().unwrap();
        assert_eq!(ev.costs[1], 7, "价 +1");
        assert_eq!(ev.screen, 0, "留在主屏可再买");
        run.choose_event_option(1);
        assert_eq!(run.player.hp, 80 - 6 - 7);
        run.choose_event_option(3); // 离开
        assert_eq!(run.player.hp, 80 - 6 - 7 - 6);
        assert!(run.event.as_ref().unwrap().done);
    }

    /// Nloth：以旧换新 Nloth's Gift
    #[test]
    fn nloth_trade() {
        let mut run = with_event(41, 0, EventKind::Nloth);
        run.relics.push(RelicInst::new(RelicKind::Lantern));
        let before = run.relics.len();
        run.choose_event_option(0);
        assert!(run.relics.iter().any(|r| r.kind == RelicKind::NlothsGift));
        assert_eq!(run.relics.len(), before, "一换一");
    }

    /// TheJoust：押 50 金，30% 庄家胜
    #[test]
    fn the_joust() {
        // 找一个庄家输的种子（押 against +100）
        for seed in 0..64u64 {
            let mut run = with_event(seed, 0, EventKind::TheJoust);
            run.choose_event_option(0);
            let gold = run.player.gold;
            if gold == 99 - 50 + 100 {
                return;
            }
            assert_eq!(gold, 99 - 50, "输局只扣 50");
        }
        panic!("64 种子未见庄家输（30% 概率异常）");
    }
}
