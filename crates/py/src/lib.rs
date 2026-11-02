//! neow-sim 的 Python 绑定：gym 接口（observation-encoding.md v2.1）。
//!
//! 用法：
//! ```python
//! import neow_sim
//! env = neow_sim.GymEnv(seed=42, ascension=20, character="IRONCLAD")
//! obs = env.reset()
//! obs, reward, done, info = env.step(action_type=0, entity=1, target=0)
//! ```

use neow_sim_core::combat::Action;
use neow_sim_core::encoding::{self, EncodedObservation};
use neow_sim_core::run::{RoomKind, RunState};
use neow_sim_core::state::CharKind;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

// ── 观测包装 ─────────────────────────────────────

#[pyclass]
struct Observation {
    #[pyo3(get)]
    token_types: Vec<i8>,
    #[pyo3(get)]
    token_ids: Vec<(i32, i32, i32)>,
    #[pyo3(get)]
    token_feats: Vec<Vec<f32>>,
    #[pyo3(get)]
    seq_len: usize,
}

impl From<EncodedObservation> for Observation {
    fn from(e: EncodedObservation) -> Self {
        Self {
            token_types: e.token_types[..e.seq_len].to_vec(),
            token_ids: e.token_ids[..e.seq_len]
                .iter()
                .map(|a| (a[0], a[1], a[2]))
                .collect(),
            token_feats: e.token_feats[..e.seq_len]
                .iter()
                .map(|f| f.to_vec())
                .collect(),
            seq_len: e.seq_len,
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
struct LegalActions {
    #[pyo3(get)]
    flat: Vec<FlatAction>,
    #[pyo3(get)]
    type_mask: Vec<bool>,
    #[pyo3(get)]
    entity_mask: Vec<bool>,
    #[pyo3(get)]
    target_mask: Vec<bool>,
}

// ── Gym 环境 ─────────────────────────────────────

#[pyclass]
struct GymEnv {
    run: RunState,
    obs: Option<Observation>,
    legal: Option<LegalActions>,
    done: bool,
    step_count: u64,
}

#[pymethods]
impl GymEnv {
    #[new]
    #[pyo3(signature = (seed, ascension=20, character="IRONCLAD"))]
    fn new(seed: u64, ascension: i32, character: &str) -> PyResult<Self> {
        let ch = CharKind::from_id(character)
            .ok_or_else(|| PyRuntimeError::new_err(format!("Unknown character: {character}")))?;
        Ok(Self {
            run: RunState::new_for(seed, ascension, ch),
            obs: None,
            legal: None,
            done: false,
            step_count: 0,
        })
    }

    /// 重置环境
    fn reset(&mut self) -> PyResult<Observation> {
        let ch = self.run.char_kind;
        let seed = self.run.seed;
        let asc = self.run.ascension;
        self.run = RunState::new_for(seed, asc, ch);
        self.done = false;
        self.step_count = 0;

        // 进第一个房间
        self.advance_to_next_decision();
        Ok(self.obs.clone().unwrap())
    }

    /// 执行动作
    #[pyo3(signature = (action_type, entity=-1, target=-1))]
    fn step(
        &mut self,
        action_type: i32,
        entity: i32,
        target: i32,
    ) -> PyResult<(Observation, f64, bool, String)> {
        if self.done {
            return Err(PyRuntimeError::new_err(
                "Environment is done. Call reset().",
            ));
        }
        self.step_count += 1;
        let reward = self.apply_action(action_type, entity, target)?;
        self.advance_to_next_decision();

        let obs = self.obs.clone().unwrap();
        let info = format!(
            "{{\"step\": {}, \"act\": {}, \"floor\": {}, \"hp\": {}}}",
            self.step_count, self.run.act, self.run.floor, self.run.player.hp
        );
        Ok((obs, reward, self.done, info))
    }

    /// 获取当前观测
    #[getter]
    fn observation(&self) -> PyResult<Observation> {
        self.obs
            .clone()
            .ok_or_else(|| PyRuntimeError::new_err("No observation. Call reset()."))
    }

    /// 获取合法动作
    #[getter]
    fn legal_actions(&self) -> PyResult<LegalActions> {
        self.legal
            .clone()
            .ok_or_else(|| PyRuntimeError::new_err("No legal actions. Call reset()."))
    }

    /// 引擎信息
    #[getter]
    fn info(&self) -> String {
        format!(
            "{{\"act\": {}, \"floor\": {}, \"hp\": {}, \"gold\": {}, \"deck\": {}, \"relics\": {}}}",
            self.run.act, self.run.floor, self.run.player.hp,
            self.run.player.gold, self.run.deck.len(), self.run.relics.len()
        )
    }
}

// ── 内部方法 ─────────────────────────────────────

impl GymEnv {
    /// 推进到下一个决策点（进房/结算/生成观测）
    fn advance_to_next_decision(&mut self) {
        if self.run.over {
            self.done = true;
            self.obs = Some(Observation {
                token_types: vec![1], // CLS
                token_ids: vec![(-1, -1, -1)],
                token_feats: vec![vec![0.0; encoding::FEAT_DIM]],
                seq_len: 1,
            });
            self.legal = Some(self.make_dummy_legal());
            return;
        }

        // 进房
        let edges = self.run.map.rows[self.run.pos.0][self.run.pos.1]
            .edges
            .len();
        if self.run.pos.0 + 1 < 15 && edges > 0 {
            self.run.choose_node(0);
        } else if self.run.pos.0 == 14 {
            // Boss
            self.run.map.rows[14][self.run.pos.1].room = Some(RoomKind::Boss);
        } else {
            self.done = true;
            return;
        }
        self.run.enter_room();
        self.run.floor += 1;

        // 编码观测
        if let Some(combat) = &self.run.combat {
            let encoded = encoding::encode_combat(combat);
            self.obs = Some(Observation::from(encoded));
            self.legal = Some(self.make_combat_legal(combat));
        } else {
            // 非战斗屏——用简化观测
            self.obs = Some(Observation {
                token_types: vec![1, 2],
                token_ids: vec![(-1, -1, -1), (-1, -1, -1)],
                token_feats: vec![vec![0.0; encoding::FEAT_DIM]; 2],
                seq_len: 2,
            });
            self.legal = Some(self.make_dummy_legal());
        }

        // 结算战斗（简化：如果战斗结束就 settle）
        if let Some(mut c) = self.run.combat.take() {
            if c.over {
                self.run.settle_combat(0);
            } else {
                self.run.combat = Some(c);
            }
        }
    }

    /// 应用动作
    fn apply_action(&mut self, _ty: i32, _entity: i32, _target: i32) -> PyResult<f64> {
        // TODO: 完整的动作分发——当前是骨架
        // 简化策略：如果是战斗中，出第一张能出的牌或 EndTurn
        if let Some(c) = self.run.combat.as_mut() {
            let action = Action::EndTurn;
            c.step(&action);
            if c.over {
                let won = c.won;
                self.run.settle_combat(0);
                if won {
                    return Ok(1.0);
                }
                return Ok(-1.0);
            }
        }
        Ok(0.0)
    }

    /// 生成战斗合法动作
    fn make_combat_legal(&self, c: &neow_sim_core::combat::Combat) -> LegalActions {
        let mut flat = Vec::new();
        // PlayCard for each hand card
        for (i, card) in c.player.hand.iter().enumerate() {
            let cost = card.cost_pay().max(0);
            if cost <= c.player.energy {
                if card.def.target == "enemy" && !c.monsters.is_empty() {
                    for mi in 0..c.monsters.len() {
                        if c.monsters[mi].creature.hp > 0 {
                            flat.push(FlatAction {
                                action_type: 0, // PlayCard
                                entity: i as i32,
                                target: mi as i32,
                            });
                        }
                    }
                } else {
                    flat.push(FlatAction {
                        action_type: 0,
                        entity: i as i32,
                        target: -1,
                    });
                }
            }
        }
        // EndTurn
        flat.push(FlatAction {
            action_type: 1,
            entity: -1,
            target: -1,
        });

        let mut type_mask = vec![false; 24];
        let mut entity_mask = vec![false; encoding::MAX_T];
        let mut target_mask = vec![false; encoding::MAX_T];

        for a in &flat {
            type_mask[a.action_type as usize] = true;
            if a.entity >= 0 {
                entity_mask[a.entity as usize] = true;
            }
            if a.target >= 0 {
                target_mask[a.target as usize] = true;
            }
        }

        LegalActions {
            flat,
            type_mask,
            entity_mask,
            target_mask,
        }
    }

    fn make_dummy_legal(&self) -> LegalActions {
        LegalActions {
            flat: vec![FlatAction {
                action_type: 14,
                entity: -1,
                target: -1,
            }], // Proceed
            type_mask: {
                let mut m = vec![false; 24];
                m[14] = true;
                m
            },
            entity_mask: vec![false; encoding::MAX_T],
            target_mask: vec![false; encoding::MAX_T],
        }
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
