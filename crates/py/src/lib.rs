//! neow-sim 的 Python 绑定：gym 接口（observation-encoding.md v2.1）。
//!
//! 完整游戏循环：地图→战斗→事件→奖励→商店→休息→Boss→转幕→终局。
//!
//! 用法：
//! ```python
//! import neow_sim
//! env = neow_sim.GymEnv(seed=42, ascension=20, character="IRONCLAD")
//! obs = env.reset()
//! while not done:
//!     legal = env.legal_actions
//!     # ... AI 决策 ...
//!     obs, reward, done, info = env.step(action_type, entity, target)
//! ```

use neow_sim_core::combat::{Action, Combat};
use neow_sim_core::encoding::{self, EncodedObservation};
use neow_sim_core::run::{RewardItem, RoomKind, RunState, MAP_HEIGHT, MAP_WIDTH};
use neow_sim_core::state::CharKind;
use numpy::{PyArray1, PyArray2};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyTuple;

// ── 动作类型常量（与 observation-encoding.md §2.2 对齐）──

pub mod action_type {
    pub const PLAY_CARD: i32 = 0;
    pub const END_TURN: i32 = 1;
    pub const USE_POTION: i32 = 2;
    pub const DISCARD_POTION: i32 = 3;
    pub const CHOOSE_MAP: i32 = 4;
    pub const CHOOSE_EVENT: i32 = 5;
    pub const REST_ACTION: i32 = 6;
    pub const SHOP_BUY: i32 = 7;
    pub const TAKE_REWARD: i32 = 8;
    pub const SKIP_REWARD: i32 = 9;
    pub const PROCEED: i32 = 14;
    pub const CANCEL: i32 = 15;
}

// ── 游戏状态机 ─────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
enum GameState {
    NeowChoice,      // 开局 Neow 事件（floor 0）
    MapNode,         // 选下一个地图节点
    Combat,          // 战斗中（出牌/结束回合）
    EventChoice,     // 事件选项
    RewardScreen,    // 奖励屏
    RestChoice,      // 休息选项
    ShopScreen,      // 商店（简化：跳过）
    BossRelicChoice, // Boss 遗物 3 选 1（可跳过）
    Done,            // 游戏结束
}

// ── 观测包装 ─────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct Observation {
    raw_types: Vec<i8>,
    raw_ids: Vec<i32>,
    raw_feats: Vec<f32>,
    seq_len: usize,
    screen: i32,
}

impl From<EncodedObservation> for Observation {
    fn from(e: EncodedObservation) -> Self {
        Self {
            raw_types: e.token_types[..e.seq_len].to_vec(),
            raw_ids: e.token_ids[..e.seq_len]
                .iter()
                .flat_map(|a| a.iter().copied())
                .collect(),
            raw_feats: e.token_feats[..e.seq_len]
                .iter()
                .flat_map(|f| f.iter().copied())
                .collect(),
            seq_len: e.seq_len,
            screen: 0,
        }
    }
}

#[pymethods]
impl Observation {
    #[getter]
    fn token_types_np<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<i8>> {
        PyArray1::from_slice(py, &self.raw_types[..self.seq_len])
    }

    #[getter]
    fn token_ids_np<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray2<i32>> {
        let rows: Vec<Vec<i32>> = self.raw_ids[..self.seq_len * 3]
            .chunks(3)
            .map(|c| vec![c[0], c[1], c[2]])
            .collect();
        PyArray2::from_vec2(py, &rows).unwrap()
    }

    #[getter]
    fn token_feats_np<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray2<f32>> {
        let rows: Vec<Vec<f32>> = self.raw_feats[..self.seq_len * encoding::FEAT_DIM]
            .chunks(encoding::FEAT_DIM)
            .map(|c| c.to_vec())
            .collect();
        PyArray2::from_vec2(py, &rows).unwrap()
    }

    #[getter]
    fn seq_len(&self) -> usize {
        self.seq_len
    }

    #[getter]
    fn screen(&self) -> i32 {
        self.screen
    }
}

impl Observation {
    fn simple(screen: i32) -> Self {
        Self {
            raw_types: vec![1, 2],
            raw_ids: vec![-1, -1, -1, -1, -1, -1],
            raw_feats: vec![0.0; 2 * encoding::FEAT_DIM],
            seq_len: 2,
            screen,
        }
    }
}

// ── 合法动作 ─────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct FlatAction {
    #[pyo3(get)]
    action_type: i32,
    #[pyo3(get)]
    entity: i32,
    #[pyo3(get)]
    target: i32,
}

#[pyclass]
#[derive(Clone)]
struct LegalActions {
    #[pyo3(get)]
    flat: Vec<FlatAction>,
    #[pyo3(get)]
    screen: i32,
}

// ── Gym 环境 ─────────────────────────────────────

#[pyclass]
struct GymEnv {
    run: RunState,
    state: GameState,
    obs: Option<Observation>,
    legal: Option<LegalActions>,
    done: bool,
    total_reward: f64,
    step_count: u64,
}

#[pymethods]
impl GymEnv {
    #[new]
    #[pyo3(signature = (seed, ascension=20, character="IRONCLAD"))]
    fn new(seed: u64, ascension: i32, character: &str) -> PyResult<Self> {
        let ch = CharKind::from_id(character)
            .ok_or_else(|| PyRuntimeError::new_err(format!("Unknown character: {character}")))?;
        let mut run = RunState::new_for(seed, ascension, ch);
        Self::fix_start_pos(&mut run);
        let mut env = Self {
            run,
            state: GameState::MapNode,
            obs: None,
            legal: None,
            done: false,
            total_reward: 0.0,
            step_count: 0,
        };
        env.enter_neow();
        Ok(env)
    }

    fn reset(&mut self) -> PyResult<Observation> {
        let ch = self.run.char_kind;
        let seed = self.run.seed;
        let asc = self.run.ascension;
        let mut run = RunState::new_for(seed, asc, ch);
        Self::fix_start_pos(&mut run);
        self.run = run;
        self.done = false;
        self.total_reward = 0.0;
        self.step_count = 0;
        self.enter_neow();
        Ok(self.obs.clone().unwrap())
    }

    #[pyo3(signature = (action_type, entity=-1, target=-1))]
    fn step(
        &mut self,
        action_type: i32,
        entity: i32,
        target: i32,
    ) -> PyResult<(Observation, f64, bool, String)> {
        if self.done {
            return Err(PyRuntimeError::new_err("Environment is done."));
        }
        self.step_count += 1;
        let step_reward = self.dispatch(action_type, entity, target)?;

        // 检查游戏是否结束
        if self.run.over {
            self.done = true;
            self.total_reward += if self.run.won { 1.0 } else { -1.0 };
        }

        let obs = self.obs.clone().unwrap();
        let info =
            format!(
            "{{\"step\":{}, \"state\":\"{:?}\", \"act\":{}, \"floor\":{}, \"hp\":{}, \"gold\":{}}}",
            self.step_count, self.state, self.run.act, self.run.floor,
            self.run.player.hp, self.run.player.gold
        );
        Ok((obs, step_reward, self.done, info))
    }

    #[getter]
    fn observation(&self) -> PyResult<Observation> {
        self.obs
            .clone()
            .ok_or_else(|| PyRuntimeError::new_err("No observation."))
    }

    #[getter]
    fn legal_actions(&self) -> PyResult<LegalActions> {
        self.legal
            .clone()
            .ok_or_else(|| PyRuntimeError::new_err("No legal actions."))
    }

    #[getter]
    fn info(&self) -> String {
        format!(
            "{{\"act\":{},\"floor\":{},\"hp\":{},\"max_hp\":{},\"gold\":{},\"deck\":{},\"relics\":{},\"potions\":{},\"state\":\"{:?}\"}}",
            self.run.act, self.run.floor, self.run.player.hp, self.run.player.max_hp,
            self.run.player.gold, self.run.deck.len(), self.run.relics.len(),
            self.run.potions.iter().filter(|p| p.is_some()).count(), self.state
        )
    }

    #[getter]
    fn is_done(&self) -> bool {
        self.done
    }

    #[getter]
    fn game_state(&self) -> String {
        format!("{:?}", self.state)
    }

    /// Neow 屏的 4 个选项（描述文本），对拍/调试用
    #[getter]
    fn neow_options(&self) -> Vec<String> {
        self.run
            .neow_option_list()
            .into_iter()
            .map(|(d, _)| d)
            .collect()
    }

    /// M4 对拍用：结构化状态快照（JSON 字符串）。
    /// 字段命名与 bridge Obs 对齐，便于逐字段 diff。
    fn state_json(&self) -> String {
        let p = &self.run.player;
        let mut o = String::from("{\"meta\":{");
        o.push_str(&format!(
            "\"pos\":[{},{}],\"hp\":{},\"max_hp\":{},\"gold\":{},\"act\":{},\"floor\":{}",
            self.run.pos.0, self.run.pos.1, p.hp, p.max_hp, p.gold, self.run.act, self.run.floor
        ));
        // RNG 计数器（对拍定位分叉的关键）
        o.push_str(",\"rng\":{");
        // 战斗中真实推进的是 combat 内的 RNG 副本（结束后才回写 run.rng）
        let r = if let Some(c) = &self.run.combat {
            &c.rng
        } else {
            &self.run.rng
        };
        let streams: [(&str, u32); 13] = [
            ("monster", r.monster.counter),
            ("map", r.map.counter),
            ("event", r.event.counter),
            ("merchant", r.merchant.counter),
            ("card", r.card.counter),
            ("treasure", r.treasure.counter),
            ("relic", r.relic.counter),
            ("potion", r.potion.counter),
            ("monster_hp", r.monster_hp.counter),
            ("ai", r.ai.counter),
            ("shuffle", r.shuffle.counter),
            ("card_random", r.card_random.counter),
            ("misc", r.misc.counter),
        ];
        for (i, (name, v)) in streams.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str(&format!("\"{}\":{}", name, v));
        }
        o.push_str("}},\"state\":\"");
        o.push_str(&format!("{:?}", self.state));
        o.push('"');

        // 玩家
        o.push_str(",\"player\":{");
        if let Some(c) = &self.run.combat {
            o.push_str(&format!(
                "\"hp\":{},\"max_hp\":{},\"block\":{},\"energy\":{},\"gold\":{},\"powers\":[",
                c.player.creature.hp,
                c.player.creature.max_hp,
                c.player.creature.block,
                c.player.energy,
                c.gold
            ));
            for (i, pw) in c.player.creature.powers.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str(&format!(
                    "{{\"id\":\"{}\",\"amount\":{}}}",
                    pw.id(),
                    pw.amount
                ));
            }
            o.push_str(&format!("],\"stance\":\"{:?}\"}}", c.player.stance));
            o.push_str(&format!(",\"turn\":{}", c.turn));

            // 手牌（显示序）
            o.push_str(",\"hand\":[");
            for (i, cd) in c.player.hand.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str(&format!(
                    "{{\"id\":\"{}\",\"upgraded\":{},\"cost_for_turn\":{}}}",
                    cd.def.id,
                    cd.upgraded,
                    cd.cost_pay()
                ));
            }
            o.push(']');

            // 怪物
            o.push_str(",\"monsters\":[");
            let mut first = true;
            for (mi, m) in c.monsters.iter().enumerate() {
                if m.creature.hp <= 0 {
                    continue;
                }
                if !first {
                    o.push(',');
                }
                first = false;
                // 意图伤害必须用**结算后**的值（含力量/虚弱等乘区）。
                // move_base_damage() 是基值，少算力量 —— 曾导致 intent 10 vs 7。
                let idmg = c.intent_damage(mi);
                o.push_str(&format!(
                    "{{\"id\":\"{}\",\"hp\":{},\"max_hp\":{},\"block\":{},\"intent\":\"{:?}\",\"intent_dmg\":{},\"powers\":[",
                    m.kind.id(), m.creature.hp, m.creature.max_hp, m.creature.block,
                    m.intent(),
                    idmg.map(|d| d.to_string()).unwrap_or_else(|| "null".into())
                ));
                for (j, pw) in m.creature.powers.iter().enumerate() {
                    if j > 0 {
                        o.push(',');
                    }
                    o.push_str(&format!(
                        "{{\"id\":\"{}\",\"amount\":{}}}",
                        pw.id(),
                        pw.amount
                    ));
                }
                o.push_str("]}");
            }
            o.push(']');

            o.push_str(&format!(
                ",\"draw_pile_n\":{},\"discard_pile_n\":{},\"exhaust_pile_n\":{}",
                c.player.draw.len(),
                c.player.discard.len(),
                c.player.exhaust.len()
            ));
            // 抽牌堆真实顺序（Vec 尾 = 堆顶 = 下一张抽到）
            o.push_str(",\"draw_order\":[");
            for (i, cd) in c.player.draw.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str(&format!("\"{}\"", cd.def.id));
            }
            o.push(']');
            o.push_str(",\"discard_order\":[");
            for (i, cd) in c.player.discard.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str(&format!("\"{}\"", cd.def.id));
            }
            o.push(']');

            o.push_str(",\"relics\":[");
            for (i, rl) in c.player_relics.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str(&format!(
                    "{{\"id\":\"{}\",\"counter\":{}}}",
                    rl.kind.id(),
                    rl.counter
                ));
            }
            o.push(']');
        } else {
            o.push_str(&format!(
                "\"hp\":{},\"max_hp\":{},\"block\":0,\"energy\":0,\"gold\":{},\"powers\":[]",
                p.hp, p.max_hp, p.gold
            ));
            o.push_str(",\"relics\":[");
            for (i, rl) in self.run.relics.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str(&format!(
                    "{{\"id\":\"{}\",\"counter\":{}}}",
                    rl.kind.id(),
                    rl.counter
                ));
            }
            o.push(']');
            o.push('}'); // 闭合非战斗分支的 player 对象
        }
        // 主牌组（顶层字段，对拍用）
        o.push_str(",\"deck\":[");
        for (i, cd) in self.run.deck.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            o.push_str(&format!(
                "{{\"id\":\"{}\",\"upgraded\":{}}}",
                cd.def.id, cd.upgraded
            ));
        }
        o.push(']');
        // 奖励项类型序列（对拍用）
        o.push_str(",\"reward_kinds\":[");
        for (i, r) in self.run.rewards.iter().enumerate() {
            if i > 0 {
                o.push(',');
            }
            let t = match r {
                RewardItem::Gold(_) => "GOLD",
                RewardItem::Card(_) => "CARD",
                RewardItem::CardReward(_) => "CARDREWARD",
                RewardItem::Potion(_) => "POTION",
                RewardItem::Relic(_) => "RELIC",
            };
            o.push_str(&format!("\"{}\"", t));
        }
        o.push(']');
        o.push('}'); // 闭合顶层
        o
    }
}

// ── 内部实现 ─────────────────────────────────────

impl GymEnv {
    /// 找有边的行 0 节点作为起点（非路径节点无边）
    fn fix_start_pos(run: &mut RunState) {
        if run.map.rows[0][run.pos.1].edges.is_empty() {
            for x in 0..7 {
                if !run.map.rows[0][x].edges.is_empty() {
                    run.pos = (0, x);
                    break;
                }
            }
        }
    }

    /// 进入下一个房间并更新状态机（非递归——用循环防止栈溢出）
    /// 进入 Neow 开局屏（floor 0）。真游戏流程：
    /// floor 0 = Neow 事件 → 选一项 → 地图 → floor 1 = 第一场战斗。
    fn enter_neow(&mut self) {
        self.run.init_neow();
        self.state = GameState::NeowChoice;
        self.update_neow_obs();
    }

    fn update_neow_obs(&mut self) {
        self.obs = Some(Observation::simple(6));
        self.legal = Some(self.make_neow_legal());
    }

    fn make_neow_legal(&self) -> LegalActions {
        let mut flat = Vec::new();
        for (i, _) in self.run.neow_option_list().iter().enumerate() {
            flat.push(FlatAction {
                action_type: action_type::CHOOSE_EVENT,
                entity: i as i32,
                target: -1,
            });
        }
        LegalActions { flat, screen: 6 }
    }

    /// 进入当前节点的房间，并按房间类型设置状态机。
    /// 调用前提：pos 已指向目标节点（choose_node 已推进 floor）。
    fn open_current_room(&mut self) {
        self.run.enter_room();
        if self.run.over {
            self.set_done();
            return;
        }
        if self.run.combat.is_some() {
            self.state = GameState::Combat;
            self.update_combat_obs();
        } else if self.run.event.is_some() {
            self.state = GameState::EventChoice;
            self.update_run_obs();
        } else if !self.run.boss_relic_candidates.is_empty() {
            self.state = GameState::BossRelicChoice;
            self.legal = Some(self.make_boss_relic_legal());
            self.update_run_obs();
        } else if !self.run.rewards.is_empty() {
            self.state = GameState::RewardScreen;
            self.update_run_obs();
        } else if self.run.rest_available {
            self.state = GameState::RestChoice;
            self.update_run_obs();
        } else if self.run.shop.is_some() {
            self.state = GameState::ShopScreen;
            self.update_run_obs();
        } else {
            // 空房间（宝箱已入奖励 / 事件已完）→ 领奖励后回地图
            self.run.take_all_rewards_skip_cards();
            self.to_map_screen();
        }
    }

    /// 回到地图屏（房间结束后的唯一合法状态）
    fn to_map_screen(&mut self) {
        if self.run.over {
            self.set_done();
            return;
        }
        self.state = GameState::MapNode;
        self.legal = Some(self.make_map_legal());
        self.update_run_obs();
    }

    /// 分发动作
    fn dispatch(&mut self, ty: i32, entity: i32, target: i32) -> PyResult<f64> {
        match self.state {
            GameState::NeowChoice => {
                let idx = entity.max(0) as usize;
                self.run.choose_neow_pick(idx);
                // Neow 结束 → 地图屏（玩家选第一层节点）
                self.state = GameState::MapNode;
                self.legal = Some(self.make_map_legal());
                self.update_run_obs();
                Ok(0.0)
            }
            GameState::BossRelicChoice => {
                if ty == action_type::SKIP_REWARD {
                    self.run.choose_boss_relic(usize::MAX);
                } else {
                    self.run.choose_boss_relic(entity.max(0) as usize);
                }
                self.update_state();
                Ok(0.0)
            }
            GameState::Combat => self.combat_step(ty, entity, target),
            GameState::EventChoice => {
                let Some(ev) = self.run.event.as_ref() else {
                    self.to_map_screen();
                    self.update_state();
                    return Ok(0.0);
                };

                // 优先处理挂起选牌/选库
                if !ev.pending.is_empty() {
                    if !self.run.deck.is_empty() {
                        self.run.event_pick_card(0);
                    }
                    // pending 可能还有后续
                    if self.run.event.as_ref().map(|e| !e.pending.is_empty()) == Some(true)
                        && !self.run.deck.is_empty()
                    {
                        self.run.event_pick_card(1);
                    }
                    // 图书馆选卡
                    if self.run.event.as_ref().map(|e| !e.library.is_empty()) == Some(true) {
                        self.run.event_library_pick(0);
                    }
                } else if !ev.library.is_empty() {
                    self.run.event_library_pick(0);
                } else {
                    // 正常选项
                    let opts = self.run.event_options();
                    let pick = if entity >= 0
                        && (entity as usize) < opts.len()
                        && !opts[entity as usize].disabled
                    {
                        entity as usize
                    } else {
                        // 选最后一个（通常是离开）
                        opts.len().saturating_sub(1)
                    };
                    self.run.choose_event_option(pick);
                }

                // 事件未完成 → 留在事件屏
                let ev_done = self.run.event.as_ref().map(|e| e.done).unwrap_or(true);
                if !ev_done && self.run.event.is_some() {
                    self.update_run_obs();
                    self.legal = Some(self.make_event_legal());
                    return Ok(0.0);
                }

                // 事件完成 → 领奖励 → 下一房
                self.run.take_all_rewards_skip_cards();
                self.to_map_screen();
                self.update_state();
                Ok(0.0)
            }
            GameState::RewardScreen => {
                // chest_open：宝箱的奖励在**进房时**已结算（AbstractChest.randomizeReward
                // + open 时 addGold/addRelic），这里是无操作。
                // ⚠️ 若把它当 TAKE_REWARD 处理会立刻领光奖励并跳回地图，
                // 后续 bridge 的 take/skip/proceed 就全落空（实测 floor 10 少 15 金币）。
                if ty == 12 {
                    return Ok(0.0);
                }
                if ty == action_type::PROCEED {
                    // 奖励屏「继续」：**不领取**剩余项（未选即放弃）。
                    // ⚠️ 不能用 take_all_rewards_skip_cards()——它会把剩余的
                    // 卡牌奖励全部塞进牌组（真游戏里卡牌奖励是「1 项含 3 选 1」，
                    // 引擎建模为 3 个独立项，全领会多塞 2 张）。
                    self.run.rewards.clear();
                    self.to_map_screen();
                    return Ok(0.0);
                }
                if ty == action_type::SKIP_REWARD {
                    // 跳过：真游戏一次只跳一项（卡牌奖励可用 Singing Bowl 等）；
                    // entity 给下标则跳过该项，否则清空。
                    if entity >= 0 && (entity as usize) < self.run.rewards.len() {
                        self.run.rewards.remove(entity as usize);
                    } else {
                        self.run.rewards.clear();
                    }
                } else {
                    // 按下标领取指定项（而非全拿）
                    //
                    // entity 编码：奖励项下标。对 CardReward，候选下标用 target
                    // （-1 = 取第 0 张）。这样「3 选 1」是可决策的，而不是强制取第一张。
                    let idx = entity.max(0) as usize;
                    if idx < self.run.rewards.len() {
                        match &self.run.rewards[idx] {
                            RewardItem::CardReward(cands) => {
                                let pick = if target >= 0 { target as usize } else { 0 };
                                if let Some(c) = cands.get(pick).cloned() {
                                    self.run.deck.push(c);
                                }
                                self.run.rewards.remove(idx);
                            }
                            _ => self.run.take_reward(idx),
                        }
                    }
                    // 如果还有奖励项，停留在奖励屏
                    if !self.run.rewards.is_empty() {
                        self.update_run_obs();
                        return Ok(0.0);
                    }
                }
                // 奖励屏空 → 离开
                self.run.take_all_rewards_skip_cards();
                self.to_map_screen();
                self.update_state();
                Ok(0.0)
            }
            GameState::RestChoice => {
                match entity {
                    0 => {
                        // sleep：回 30% HP
                        self.run.rest_sleep();
                    }
                    1 => {
                        // smith：升级第一张可升级牌
                        if let Some(idx) = self.run.deck_upgradable().first() {
                            self.run.rest_smith(*idx);
                        } else {
                            // 无可升级 → 退回睡觉
                            self.run.rest_sleep();
                        }
                    }
                    2 => {
                        // toke：删第一张牌
                        if let Some(idx) = self.run.deck_purgeable().first() {
                            self.run.deck.remove(*idx);
                        }
                    }
                    _ => {
                        self.run.rest_sleep(); // 默认睡觉
                    }
                }
                self.to_map_screen();
                self.update_state();
                Ok(0.0)
            }
            GameState::ShopScreen => {
                let Some(shop) = self.run.shop.as_mut() else {
                    self.to_map_screen();
                    self.update_state();
                    return Ok(0.0);
                };
                match ty {
                    action_type::SHOP_BUY => {
                        let idx = entity.max(0) as usize;
                        // entity 编码：0-4=色卡, 5-6=无色, 7-9=遗物, 10-12=药水, 13=删卡
                        match idx {
                            0..=4 => {
                                let _ = self.run.shop_buy_card(idx);
                            }
                            5..=6 => {
                                let _ = self.run.shop_buy_colorless(idx - 5);
                            }
                            7..=9 => {
                                let _ = self.run.shop_buy_relic(idx - 7);
                            }
                            10..=12 => {
                                let _ = self.run.shop_buy_potion(idx - 10);
                            }
                            13 => {
                                // 删卡：选第一张可删牌
                                let purge_idx = self
                                    .run
                                    .deck
                                    .iter()
                                    .position(|c| !c.def.id.starts_with("Strike"))
                                    .unwrap_or(0);
                                let _ = self.run.shop_purge(purge_idx);
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        // 离开商店
                        self.to_map_screen();
                        self.update_state();
                        return Ok(0.0);
                    }
                }
                // 更新商店观测（物品已售出后变化）
                self.update_run_obs();
                Ok(0.0)
            }
            GameState::MapNode => {
                // entity = 出边下标（0 也是合法边，不能当成"不选"）
                let (y, x) = self.run.pos;
                if !self.run.map_started {
                    // 首次选点：entity = row 0 的列
                    let cols = self.run.start_columns();
                    let col = if entity >= 0 && (entity as usize) < MAP_WIDTH {
                        entity as usize
                    } else {
                        *cols.first().unwrap_or(&0)
                    };
                    self.run.pos = (0, col);
                    self.run.map_started = true;
                    self.run.floor += 1;
                    self.open_current_room();
                    return Ok(0.0);
                }
                let edges = if y < MAP_HEIGHT {
                    self.run.map.rows[y][x].edges.len()
                } else {
                    0
                };
                let edge = if entity >= 0 && (entity as usize) < edges {
                    entity as usize
                } else {
                    0
                };
                if edges > 0 {
                    self.run.choose_node(edge);
                    self.open_current_room();
                } else if y == MAP_HEIGHT - 1 {
                    // Boss 行：直接进 Boss 房
                    self.run.map.rows[y][x].room = Some(RoomKind::Boss);
                    self.open_current_room();
                } else {
                    self.set_done();
                }
                Ok(0.0)
            }
            GameState::Done => Ok(0.0),
        }
    }

    /// 战斗内动作
    fn combat_step(&mut self, ty: i32, entity: i32, target: i32) -> PyResult<f64> {
        let Some(mut c) = self.run.combat.take() else {
            self.to_map_screen();
            self.update_state();
            return Ok(0.0);
        };

        let action = match ty {
            action_type::PLAY_CARD => {
                let idx = entity.max(0) as usize;
                let tgt = if target >= 0 {
                    Some(target as usize)
                } else {
                    None
                };
                Action::PlayCard {
                    index: idx,
                    target: tgt,
                }
            }
            action_type::USE_POTION => {
                let idx = entity.max(0) as usize;
                let tgt = if target >= 0 {
                    Some(target as usize)
                } else {
                    None
                };
                Action::UsePotion {
                    index: idx,
                    target: tgt,
                }
            }
            _ => Action::EndTurn,
        };

        c.step(&action);

        // 处理挂起屏（简化：自动选择第一项）
        if c.pending.is_some() {
            use neow_sim_core::combat::Pending;
            match c.pending.as_ref().unwrap() {
                Pending::Scry { .. } => c.step(&Action::ScryChoice {
                    discard_indices: vec![],
                }),
                Pending::HandPick { .. } => c.step(&Action::HandPickResponse { index: 0 }),
                Pending::PilePick { .. } => c.step(&Action::PilePickResponse { index: 0 }),
                Pending::Discovery { .. } => c.step(&Action::DiscoveryResponse { index: 0 }),
                Pending::StanceChoice => c.step(&Action::StanceResponse { wrath: false }),
                _ => c.step(&Action::HandSelect { indices: vec![0] }),
            }
        }

        if c.over {
            let won = c.won;
            self.run.combat = Some(c);
            self.run.settle_combat(0);
            if self.run.over {
                self.set_done();
                let r = if self.run.won { 1.0 } else { -1.0 };
                return Ok(r);
            }
            // 战斗结束 → **呈现奖励屏**（对齐真游戏：不自动领取）
            // 有奖励项 → RewardScreen（由 TAKE_REWARD/SKIP_REWARD/PROCEED 决策）；
            // 无奖励项（如烟雾弹逃离）→ 直接回地图。
            if !self.run.rewards.is_empty() {
                self.state = GameState::RewardScreen;
                self.update_run_obs();
                Ok(0.0)
            } else {
                self.to_map_screen();
                Ok(0.0)
            }
        } else {
            self.run.combat = Some(c);
            self.update_combat_obs();
            Ok(0.0)
        }
    }

    /// 更新状态机到当前局面
    fn update_state(&mut self) {
        if self.run.over {
            self.set_done();
        } else if self.run.combat.is_some() {
            self.state = GameState::Combat;
            self.update_combat_obs();
        } else {
            // 非战斗——回地图屏等玩家选下一节点
            self.to_map_screen();
        }
    }

    /// Boss 遗物选择：逐项 + 跳过（真游戏 BossRelicScreen 可跳过）
    fn make_boss_relic_legal(&self) -> LegalActions {
        let mut flat = Vec::new();
        for i in 0..self.run.boss_relic_candidates.len() {
            flat.push(FlatAction {
                action_type: action_type::CHOOSE_EVENT,
                entity: i as i32,
                target: -1,
            });
        }
        flat.push(FlatAction {
            action_type: action_type::SKIP_REWARD,
            entity: -1,
            target: -1,
        });
        LegalActions { flat, screen: 7 }
    }

    /// 地图屏合法动作：未起步时列 row 0 可选列；已起步则列出当前节点的出边
    fn make_map_legal(&self) -> LegalActions {
        let mut flat = Vec::new();
        if !self.run.map_started {
            for x in self.run.start_columns() {
                flat.push(FlatAction {
                    action_type: action_type::CHOOSE_MAP,
                    entity: x as i32,
                    target: -1,
                });
            }
        } else {
            // 用 next_columns（含 WingedGreaves 的跨节点可达）。
            // entity = 下标（前 edges.len() 个是普通边，之后是飞行目标），
            // target = 目标列。
            let cols = self.run.next_columns();
            let n_edges = self.run.map.rows[self.run.pos.0][self.run.pos.1]
                .edges
                .len();
            for (i, dst_x) in cols.iter().enumerate() {
                let is_flight = i >= n_edges;
                flat.push(FlatAction {
                    action_type: action_type::CHOOSE_MAP,
                    entity: i as i32,
                    target: *dst_x as i32,
                });
                let _ = is_flight;
            }
        }
        if flat.is_empty() {
            flat.push(FlatAction {
                action_type: action_type::PROCEED,
                entity: -1,
                target: -1,
            });
        }
        LegalActions { flat, screen: 1 }
    }

    /// 事件屏合法动作
    fn make_event_legal(&self) -> LegalActions {
        let opts = self.run.event_options();
        let mut flat = Vec::new();
        for (i, opt) in opts.iter().enumerate() {
            if !opt.disabled {
                flat.push(FlatAction {
                    action_type: action_type::CHOOSE_EVENT,
                    entity: i as i32,
                    target: -1,
                });
            }
        }
        LegalActions { flat, screen: 2 }
    }

    // ── 观测生成 ─────────────────────────────────

    fn update_combat_obs(&mut self) {
        if let Some(c) = &self.run.combat {
            let mut obs = Observation::from(encoding::encode_combat(c));
            obs.screen = 0;
            self.obs = Some(obs);
            self.legal = Some(self.make_combat_legal(c));
        }
    }

    fn update_run_obs(&mut self) {
        let mut obs = Observation::from(encoding::encode_run(&self.run));
        obs.screen = match self.state {
            GameState::EventChoice => 2,
            GameState::RewardScreen => 3,
            GameState::RestChoice => 4,
            GameState::ShopScreen => 5,
            _ => 1, // map
        };
        self.obs = Some(obs);
        self.legal = Some(self.make_run_legal());
    }

    fn make_run_legal(&self) -> LegalActions {
        match self.state {
            GameState::EventChoice => {
                let n = self.run.event_options().len();
                let mut flat = Vec::new();
                for i in 0..n {
                    flat.push(FlatAction {
                        action_type: action_type::CHOOSE_EVENT,
                        entity: i as i32,
                        target: -1,
                    });
                }
                LegalActions { flat, screen: 2 }
            }
            GameState::RewardScreen => {
                // 逐项列出：take(i) / skip(i) / proceed
                let n = self.run.rewards.len();
                let mut flat = Vec::with_capacity(n * 2 + 1);
                for i in 0..n {
                    // CardReward：每个候选单独一个动作（entity=奖励下标, target=候选下标）
                    // 这样「3 选 1」与「跳过」都在动作空间里
                    match &self.run.rewards[i] {
                        RewardItem::CardReward(cands) => {
                            for (k, _) in cands.iter().enumerate() {
                                flat.push(FlatAction {
                                    action_type: action_type::TAKE_REWARD,
                                    entity: i as i32,
                                    target: k as i32,
                                });
                            }
                        }
                        _ => flat.push(FlatAction {
                            action_type: action_type::TAKE_REWARD,
                            entity: i as i32,
                            target: -1,
                        }),
                    }
                }
                for i in 0..n {
                    flat.push(FlatAction {
                        action_type: action_type::SKIP_REWARD,
                        entity: i as i32,
                        target: -1,
                    });
                }
                flat.push(FlatAction {
                    action_type: action_type::PROCEED,
                    entity: -1,
                    target: -1,
                });
                LegalActions { flat, screen: 3 }
            }
            GameState::RestChoice => LegalActions {
                flat: vec![
                    FlatAction {
                        action_type: action_type::REST_ACTION,
                        entity: 0,
                        target: -1,
                    },
                    FlatAction {
                        action_type: action_type::REST_ACTION,
                        entity: 1,
                        target: -1,
                    },
                ],
                screen: 4,
            },
            GameState::ShopScreen => LegalActions {
                flat: vec![FlatAction {
                    action_type: action_type::PROCEED,
                    entity: -1,
                    target: -1,
                }],
                screen: 5,
            },
            GameState::MapNode => self.make_map_legal(),
            _ => LegalActions {
                flat: vec![FlatAction {
                    action_type: action_type::PROCEED,
                    entity: -1,
                    target: -1,
                }],
                screen: 1,
            },
        }
    }

    fn set_done(&mut self) {
        self.state = GameState::Done;
        self.obs = Some(Observation::simple(9));
        self.legal = Some(LegalActions {
            flat: vec![],
            screen: 9,
        });
    }

    // ── 合法动作生成 ─────────────────────────────

    fn make_combat_legal(&self, c: &Combat) -> LegalActions {
        let mut flat = Vec::new();
        for (i, card) in c.player.hand.iter().enumerate() {
            if !neow_sim_core::cards::engine_playable(card.def) {
                continue;
            }
            let cost = card.cost_pay().max(0);
            if cost > c.player.energy {
                continue;
            }
            if Combat::needs_target(card) {
                for (m, mon) in c.monsters.iter().enumerate() {
                    if !mon.creature.is_dead() {
                        flat.push(FlatAction {
                            action_type: action_type::PLAY_CARD,
                            entity: i as i32,
                            target: m as i32,
                        });
                    }
                }
            } else {
                flat.push(FlatAction {
                    action_type: action_type::PLAY_CARD,
                    entity: i as i32,
                    target: -1,
                });
            }
        }
        flat.push(FlatAction {
            action_type: action_type::END_TURN,
            entity: -1,
            target: -1,
        });

        // 药水
        for (i, slot) in c.player.potions.iter().enumerate() {
            if let Some(p) = slot {
                if p.kind.can_use(c.turn_has_ended) {
                    if p.kind.target_required() {
                        for m in 0..c.monsters.len() {
                            if c.monsters[m].creature.hp > 0 {
                                flat.push(FlatAction {
                                    action_type: action_type::USE_POTION,
                                    entity: i as i32,
                                    target: m as i32,
                                });
                            }
                        }
                    } else {
                        flat.push(FlatAction {
                            action_type: action_type::USE_POTION,
                            entity: i as i32,
                            target: -1,
                        });
                    }
                }
                flat.push(FlatAction {
                    action_type: action_type::DISCARD_POTION,
                    entity: i as i32,
                    target: -1,
                });
            }
        }

        LegalActions { flat, screen: 0 }
    }
}

#[pymodule]
fn neow_sim(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("MAX_T", encoding::MAX_T)?;
    m.add("FEAT_DIM", encoding::FEAT_DIM)?;
    m.add_class::<GymEnv>()?;
    m.add_class::<Observation>()?;
    m.add_class::<LegalActions>()?;
    m.add_class::<FlatAction>()?;
    Ok(())
}
