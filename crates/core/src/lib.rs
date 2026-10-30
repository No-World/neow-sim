//! neow-sim-core：杀戮尖塔纯规则引擎（无 IO、确定性、种子可控）。
//!
//! 里程碑（见 neow-sim/README.md）：
//! M1 卡牌原语（伤害/格挡/状态计算管线）
//! M2 单场战斗闭环（IRONCLAD vs JawWorm）
//! M3 整局闭环（地图/事件/商店/休息/奖励）
//! M4 与真游戏（neow-bridge）对拍 100% 一致
//!
//! 计划模块：
//! - `state`：完整游戏状态（协议 v0.1 对应的结构体）
//! - `rng`：种子化的 RNG（对齐游戏随机流，M4 前需逆向确认）
//! - `combat`：战斗状态机（回合/抽牌/能量/意图）
//! - `cards` / `relics` / `monsters`：效果实现表
//! - `map` / `events`：局外流程
