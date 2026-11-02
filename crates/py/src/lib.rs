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
use neow_sim_core::run::{RewardItem, RoomKind, RunState, MAP_HEIGHT};
use neow_sim_core::state::CharKind;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

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
    MapNode,     // 选下一个地图节点
    Combat,      // 战斗中（出牌/结束回合）
    EventChoice, // 事件选项
    RewardScreen,// 奖励屏
    RestChoice,  // 休息选项
    ShopScreen,  // 商店（简化：跳过）
    Done,        // 游戏结束
}

// ── 观测包装 ─────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct Observation {
    #[pyo3(get)]
    token_types: Vec<i8>,
    #[pyo3(get)]
    token_ids: Vec<(i32, i32, i32)>,
    #[pyo3(get)]
    token_feats: Vec<Vec<f32>>,
    #[pyo3(get)]
    seq_len: usize,
    /// 屏幕类型：0=combat 1=map 2=event 3=reward 4=rest 5=done
    #[pyo3(get)]
    screen: i32,
}

impl From<EncodedObservation> for Observation {
    fn from(e: EncodedObservation) -> Self {
        Self {
            token_types: e.token_types[..e.seq_len].to_vec(),
            token_ids: e.token_ids[..e.seq_len].iter().map(|a| (a[0], a[1], a[2])).collect(),
            token_feats: e.token_feats[..e.seq_len].iter().map(|f| f.to_vec()).collect(),
            seq_len: e.seq_len,
            screen: 0,
        }
    }
}

impl Observation {
    fn simple(screen: i32) -> Self {
        Self {
            token_types: vec![1, 2],
            token_ids: vec![(-1, -1, -1), (-1, -1, -1)],
            token_feats: vec![vec![0.0; encoding::FEAT_DIM]; 2],
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
        env.enter_next_room();
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
        self.enter_next_room();
        Ok(self.obs.clone().unwrap())
    }

    #[pyo3(signature = (action_type, entity=-1, target=-1))]
    fn step(&mut self, action_type: i32, entity: i32, target: i32)
        -> PyResult<(Observation, f64, bool, String)>
    {
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
        let info = format!(
            "{{\"step\":{}, \"state\":\"{:?}\", \"act\":{}, \"floor\":{}, \"hp\":{}, \"gold\":{}}}",
            self.step_count, self.state, self.run.act, self.run.floor,
            self.run.player.hp, self.run.player.gold
        );
        Ok((obs, step_reward, self.done, info))
    }

    #[getter]
    fn observation(&self) -> PyResult<Observation> {
        self.obs.clone().ok_or_else(|| PyRuntimeError::new_err("No observation."))
    }

    #[getter]
    fn legal_actions(&self) -> PyResult<LegalActions> {
        self.legal.clone().ok_or_else(|| PyRuntimeError::new_err("No legal actions."))
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
    fn is_done(&self) -> bool { self.done }

    #[getter]
    fn game_state(&self) -> String { format!("{:?}", self.state) }
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
    fn enter_next_room(&mut self) {
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 100 {
                self.set_done();
                return;
            }
            if self.run.over {
                self.set_done();
                return;
            }

            // 选边 0 前进
            let pos = self.run.pos;
            let edges = self.run.map.rows[pos.0][pos.1].edges.len();
            if pos.0 + 1 < MAP_HEIGHT && edges > 0 {
                self.run.choose_node(0);
            } else if pos.0 == MAP_HEIGHT - 1 {
                // Boss 行
                self.run.map.rows[pos.0][pos.1].room = Some(RoomKind::Boss);
            } else {
                // 无法前进——游戏结束
                self.set_done();
                return;
            }
            self.run.enter_room();
            self.run.floor += 1;

            // 根据房间类型设置状态
            if self.run.combat.is_some() {
                self.state = GameState::Combat;
                self.update_combat_obs();
                return;
            } else if self.run.event.is_some() {
                self.state = GameState::EventChoice;
                self.update_event_obs();
                return;
            } else if !self.run.rewards.is_empty() {
                self.state = GameState::RewardScreen;
                self.update_reward_obs();
                return;
            } else if self.run.rest_available {
                self.state = GameState::RestChoice;
                self.update_rest_obs();
                return;
            } else if self.run.shop.is_some() {
                self.state = GameState::ShopScreen;
                self.update_shop_obs();
                return;
            }
            // 空房间 → 领奖励 → 继续循环到下一房
            self.run.take_all_rewards_skip_cards();
        }
    }

    /// 分发动作
    fn dispatch(&mut self, ty: i32, entity: i32, target: i32) -> PyResult<f64> {
        match self.state {
            GameState::Combat => self.combat_step(ty, entity, target),
            GameState::EventChoice => {
                self.run.choose_event_option(entity.max(0) as usize);
                // 事件内选牌（如果有 pending）
                if let Some(ev) = self.run.event.as_ref() {
                    if !ev.pending.is_empty() && !self.run.deck.is_empty() {
                        self.run.event_pick_card(0);
                    }
                }
                // 事件结束 → 领奖励 → 下一房
                self.run.take_all_rewards_skip_cards();
                self.enter_next_room();
                self.update_state();
                Ok(0.0)
            }
            GameState::RewardScreen => {
                if ty == action_type::SKIP_REWARD {
                    self.run.rewards.clear();
                } else {
                    self.run.take_all_rewards_skip_cards();
                }
                self.enter_next_room();
                self.update_state();
                Ok(0.0)
            }
            GameState::RestChoice => {
                match entity {
                    0 => self.run.rest_sleep(),
                    _ => {
                        if !self.run.deck.is_empty() {
                            self.run.rest_smith(0);
                        }
                    }
                }
                self.enter_next_room();
                self.update_state();
                Ok(0.0)
            }
            GameState::ShopScreen => {
                // 简化：跳过商店
                self.enter_next_room();
                self.update_state();
                Ok(0.0)
            }
            GameState::MapNode => {
                // 改变选边（如果 entity > 0）
                if entity > 0 {
                    let edges = self.run.map.rows[self.run.pos.0][self.run.pos.1].edges.len();
                    if (entity as usize) < edges {
                        self.run.choose_node(entity as usize);
                        self.run.enter_room();
                        self.run.floor += 1;
                    }
                }
                self.update_state();
                Ok(0.0)
            }
            GameState::Done => Ok(0.0),
        }
    }

    /// 战斗内动作
    fn combat_step(&mut self, ty: i32, entity: i32, target: i32) -> PyResult<f64> {
        let Some(mut c) = self.run.combat.take() else {
            self.enter_next_room();
            self.update_state();
            return Ok(0.0);
        };

        let action = match ty {
            action_type::PLAY_CARD => {
                let idx = entity.max(0) as usize;
                let tgt = if target >= 0 { Some(target as usize) } else { None };
                Action::PlayCard { index: idx, target: tgt }
            }
            action_type::USE_POTION => {
                let idx = entity.max(0) as usize;
                let tgt = if target >= 0 { Some(target as usize) } else { None };
                Action::UsePotion { index: idx, target: tgt }
            }
            _ => Action::EndTurn,
        };

        c.step(&action);

        // 处理挂起屏（简化：自动选择第一项）
        if c.pending.is_some() {
            use neow_sim_core::combat::Pending;
            match c.pending.as_ref().unwrap() {
                Pending::Scry { .. } => {
                    c.step(&Action::ScryChoice { discard_indices: vec![] })
                }
                Pending::HandPick { .. } => {
                    c.step(&Action::HandPickResponse { index: 0 })
                }
                Pending::PilePick { .. } => {
                    c.step(&Action::PilePickResponse { index: 0 })
                }
                Pending::Discovery { .. } => {
                    c.step(&Action::DiscoveryResponse { index: 0 })
                }
                Pending::StanceChoice => {
                    c.step(&Action::StanceResponse { wrath: false })
                }
                _ => {
                    c.step(&Action::HandSelect { indices: vec![0] })
                }
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
            // 战斗结束 → 领奖励 → 下一房
            self.run.take_all_rewards_skip_cards();
            self.enter_next_room();
            self.update_state();
            Ok(0.0)
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
            // 非战斗——进入下一个决策点
            self.enter_next_room();
        }
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

    fn update_event_obs(&mut self) {
        let opts = self.run.event_options();
        let n = opts.len().max(1);
        let mut legal = Vec::new();
        for i in 0..n {
            legal.push(FlatAction { action_type: action_type::CHOOSE_EVENT, entity: i as i32, target: -1 });
        }
        self.obs = Some(Observation::simple(2));
        self.legal = Some(LegalActions { flat: legal, screen: 2 });
    }

    fn update_reward_obs(&mut self) {
        let n = self.run.rewards.len();
        let mut legal = vec![
            FlatAction { action_type: action_type::TAKE_REWARD, entity: 0, target: -1 },
            FlatAction { action_type: action_type::SKIP_REWARD, entity: -1, target: -1 },
        ];
        let _ = n;
        self.obs = Some(Observation::simple(3));
        self.legal = Some(LegalActions { flat: legal, screen: 3 });
    }

    fn update_rest_obs(&mut self) {
        self.obs = Some(Observation::simple(4));
        self.legal = Some(LegalActions {
            flat: vec![
                FlatAction { action_type: action_type::REST_ACTION, entity: 0, target: -1 }, // sleep
                FlatAction { action_type: action_type::REST_ACTION, entity: 1, target: -1 }, // smith
            ],
            screen: 4,
        });
    }

    fn update_shop_obs(&mut self) {
        self.obs = Some(Observation::simple(5));
        self.legal = Some(LegalActions {
            flat: vec![FlatAction { action_type: action_type::PROCEED, entity: -1, target: -1 }],
            screen: 5,
        });
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
            if !neow_sim_core::cards::engine_playable(card.def) { continue; }
            let cost = card.cost_pay().max(0);
            if cost > c.player.energy { continue; }
            if Combat::needs_target(card) {
                for (m, mon) in c.monsters.iter().enumerate() {
                    if !mon.creature.is_dead() {
                        flat.push(FlatAction { action_type: action_type::PLAY_CARD, entity: i as i32, target: m as i32 });
                    }
                }
            } else {
                flat.push(FlatAction { action_type: action_type::PLAY_CARD, entity: i as i32, target: -1 });
            }
        }
        flat.push(FlatAction { action_type: action_type::END_TURN, entity: -1, target: -1 });

        // 药水
        for (i, slot) in c.player.potions.iter().enumerate() {
            if let Some(p) = slot {
                if p.kind.can_use(c.turn_has_ended) {
                    if p.kind.target_required() {
                        for m in 0..c.monsters.len() {
                            if c.monsters[m].creature.hp > 0 {
                                flat.push(FlatAction { action_type: action_type::USE_POTION, entity: i as i32, target: m as i32 });
                            }
                        }
                    } else {
                        flat.push(FlatAction { action_type: action_type::USE_POTION, entity: i as i32, target: -1 });
                    }
                }
                flat.push(FlatAction { action_type: action_type::DISCARD_POTION, entity: i as i32, target: -1 });
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
