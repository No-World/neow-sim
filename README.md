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
    - [x] 药水系统 42/43（PotionSlot 占位除外；掉落采样原语就绪，接入奖励层待 M3）
    - [x] 商店（进货 AASSP/无色×2/遗物×3/药水×3 + 定价 + 删卡服务；遗物具体件取自己实现池——tier 池元数据待补）
    - [x] 事件第一批：框架（generateEvent 0.25 神龛分支/池内不重复/条件门槛）+ Act1 全量 11 只 + 通用神龛 5 只 + 一次性 7 只（Act2/3 专属与 Match and Keep! 待后续批次）
    - [x] 事件第二批：Act2 全量 12 只（CursedTome 除外：Necronomicon 卡不在数据层）+ Act3 全量 6 只 + 城市专属一次性 5 只（Designer/Duplicator/Knowing Skull/N'loth/The Joust）；双波次战斗（Colosseum）与事件内多步选牌队列
    - [x] 休息房：睡觉 30% / 锻造升级（决策面属协议层）
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

## 许可与声明

本项目原创代码以 [MIT License](LICENSE) 授权；`data/` 游戏数据提取自《Slay the Spire》，版权归 Mega Crit 所有，不受 MIT 约束，详见 [LICENSE](LICENSE)。使用需持有正版游戏。
