# neow-sim — 杀戮尖塔高速规则引擎（Rust）

独立子项目：**不依赖 Neow 其他部分**，目标是做成社区可复用的 STS "服务端"。

## 目标

- 确定性：种子可控，同 seed + 同动作序列 ⇒ 同轨迹（协议见 `../docs/protocol.md`）
- 性能：≥1000 局/秒/核（release），支撑 RL 海量 rollout
- 纯规则、无 IO：核心 crate 零依赖，绑定层才暴露 Python
- 对照实现：`sts_lightspeed`（C++，现有最强 bot）与真游戏（经 neow-bridge 对拍）

## 里程碑

- [ ] **M1** 卡牌原语：伤害/格挡/力量/虚弱/易伤的计算管线
- [ ] **M2** 单场战斗闭环：铁甲战士 vs 大颚虫（Strike/Defend/Bash 起手）
- [ ] **M3** 整局闭环：地图/事件/商店/休息/奖励/遗物/药水
- [ ] **M4** 对拍通过：与 neow-bridge 黄金用例 100% 一致

## 结构

```
neow-sim/
├── crates/core/    # 纯规则引擎（零依赖，可移植 wasm）
├── crates/py/      # pyo3 绑定：pip 装 wheel 即得 gym 接口
└── tests/          # conformance（黄金用例）+ replays
```

## 使用（Python 侧）

```python
from neow_sim import Env
env = Env(seed=12345, character="IRONCLAD", ascension=20)
obs, legal = env.reset()
obs, legal, r, done, info = env.step({"type": "play", "card_index": 0, "target": 0})
```
