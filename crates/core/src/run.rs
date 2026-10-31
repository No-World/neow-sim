//! M3 局外层·一：地图生成（MapGenerator.java + RoomTypeAssigner.java 逐行移植）。
//!
//! 源码对照：
//! - `MapGenerator.generateDungeon(15, 7, 6, mapRng)`：6 条路径自底向上生长；
//!   祖先交叉规则（gap<3 改道 / gap<5 跳过）；左右邻居最值边约束防交叉；
//!   首行冗余边过滤
//! - `AbstractDungeon.generateMap`：行 0=战斗、行 8=宝箱、行 14=休息 + Boss 边；
//!   按幕概率表生成类型池（A1+ 精英 ×1.6）→ `Collections.shuffle(mapRng)`
//! - `RoomTypeAssigner`：行≤4 禁休息/精英；行≥13 禁休息；父/兄同型禁排
//!   （父适用：休息/宝箱/商店/精英；兄适用：休息/战斗/事件/精英/商店）；
//!   不可放节点跳过；漏网节点 `lastMinuteNodeChecker` 补战斗房

use crate::rng::StsRandom;

pub const MAP_HEIGHT: usize = 15;
pub const MAP_WIDTH: usize = 7;
pub const PATH_DENSITY: usize = 6;
/// 倒数第二行（Boss 前）：节点不计入类型池（源码 y == size-2 跳过）
pub const PRE_BOSS_ROW: usize = MAP_HEIGHT - 2;

/// 房间类型（AbstractRoom 子类对照）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoomKind {
    Monster,
    Elite,
    Rest,
    Treasury,
    Shop,
    Event,
    Boss,
}

/// 地图节点。edges：下一行目标列（升序 = EdgeComparator）；
/// parents：已登记的父列（MapRoomNode.addParent，可含重复）
#[derive(Clone, Debug, Default)]
pub struct MapNode {
    pub x: usize,
    pub y: usize,
    pub edges: Vec<usize>,
    pub parents: Vec<usize>,
    pub room: Option<RoomKind>,
}

#[derive(Clone, Debug)]
pub struct GameMap {
    pub rows: Vec<Vec<MapNode>>,
    pub act: u8,
}

impl GameMap {
    /// 行 14 有边的列 = 通往 Boss 的入口
    pub fn boss_columns(&self) -> Vec<usize> {
        self.rows[MAP_HEIGHT - 1]
            .iter()
            .filter(|n| !n.edges.is_empty())
            .map(|n| n.x)
            .collect()
    }

    /// 调试输出：ASCII 地图（对照 MapGenerator.toString）
    pub fn to_ascii(&self) -> String {
        let mut out = String::new();
        for row_num in (0..self.rows.len()).rev() {
            out.push('\n');
            for n in &self.rows[row_num] {
                let (mut l, mut m, mut r) = (' ', ' ', ' ');
                for &e in &n.edges {
                    if e < n.x {
                        l = '\\';
                    }
                    if e == n.x {
                        m = '|';
                    }
                    if e > n.x {
                        r = '/';
                    }
                }
                out.push(l);
                out.push(m);
                out.push(r);
            }
        }
        out
    }
}

fn rand_range(rng: &mut StsRandom, min: i32, max: i32) -> i32 {
    rng.random_range(max - min) + min
}

/// MapGenerator.generateDungeon + AbstractDungeon.generateMap
pub fn generate_dungeon(act: u8, ascension: i32, rng: &mut StsRandom) -> GameMap {
    let mut rows = create_nodes();
    create_paths(&mut rows, PATH_DENSITY, rng);
    filter_redundant_edges(&mut rows);
    assign_room_types(act, ascension, &mut rows, rng);
    GameMap { rows, act }
}

fn create_nodes() -> Vec<Vec<MapNode>> {
    (0..MAP_HEIGHT)
        .map(|y| {
            (0..MAP_WIDTH)
                .map(|x| MapNode {
                    x,
                    y,
                    ..Default::default()
                })
                .collect()
        })
        .collect()
}

/// createPaths：6 条路径；第 2 条起点不得与首条相同（while 重掷）
fn create_paths(rows: &mut [Vec<MapNode>], path_density: usize, rng: &mut StsRandom) {
    let row_size = MAP_WIDTH as i32 - 1;
    let mut first_starting_node = -1;
    for i in 0..path_density {
        let mut starting_node = rand_range(rng, 0, row_size);
        if i == 0 {
            first_starting_node = starting_node;
        }
        while starting_node == first_starting_node && i == 1 {
            starting_node = rand_range(rng, 0, row_size);
        }
        create_paths_rec(rows, starting_node, 0, rng);
    }
}

/// _createPaths 递归：到底挂 Boss 边哨兵；否则选下一格（防交叉 + 祖先规则）
fn create_paths_rec(rows: &mut [Vec<MapNode>], start_x: i32, start_y: usize, rng: &mut StsRandom) {
    // 到顶：Boss 边（dstY+2=15；哨兵 = 值 3）
    if start_y + 1 >= rows.len() {
        rows[start_y][start_x as usize].edges.push(3);
        return;
    }
    let row_end = MAP_WIDTH as i32 - 1;
    let (min, max) = if start_x == 0 {
        (0, 1)
    } else if start_x == row_end {
        (-1, 0)
    } else {
        (-1, 1)
    };
    let mut new_x = start_x + rand_range(rng, min, max);
    let new_y = start_y + 1;

    // 祖先交叉规则：对候选点的每个已登记父（不含当前节点本身）
    let mut candidate_parents: Vec<usize> = rows[new_y][new_x as usize].parents.clone();
    candidate_parents.retain(|&p| p != start_x as usize);
    'parents: for parent_x in candidate_parents {
        if let Some(ancestor_y) =
            common_ancestor(rows, (parent_x as i32, new_y - 1), (start_x, start_y), 5)
        {
            let gap = new_y as i32 - ancestor_y as i32;
            if gap < 3 {
                // 改道三分支（对照源码）
                // 三分支改道（源码 if/else if/else——用 match 保持结构清晰）
                new_x = match new_x.cmp(&start_x) {
                    std::cmp::Ordering::Greater => {
                        let x = start_x + rand_range(rng, -1, 0);
                        if x < 0 {
                            start_x
                        } else {
                            x
                        }
                    }
                    std::cmp::Ordering::Equal => {
                        let mut x = start_x + rand_range(rng, -1, 1);
                        if x > row_end {
                            x = start_x - 1;
                        } else if x < 0 {
                            x = start_x + 1;
                        }
                        x
                    }
                    std::cmp::Ordering::Less => {
                        let x = start_x + rand_range(rng, 0, 1);
                        if x > row_end {
                            start_x
                        } else {
                            x
                        }
                    }
                };
                continue 'parents; // 换候选父继续检查（源码 continue）
            }
            // gap < 5：跳过该父继续看下一个（源码空 continue）
        }
    }

    // 左右邻居最值约束（防交叉）
    if start_x != 0 {
        let left = &rows[start_y][(start_x - 1) as usize];
        if !left.edges.is_empty() {
            if let Some(&mx) = left.edges.iter().max() {
                if mx as i32 > new_x {
                    new_x = mx as i32;
                }
            }
        }
    }
    if start_x < row_end {
        let right = &rows[start_y][(start_x + 1) as usize];
        if !right.edges.is_empty() {
            if let Some(&mn) = right.edges.iter().min() {
                if (mn as i32) < new_x {
                    new_x = mn as i32;
                }
            }
        }
    }

    // 加边 + 记父
    let dst = new_x as usize;
    let cur = &mut rows[start_y][start_x as usize];
    cur.edges.push(dst);
    cur.edges.sort_unstable();
    rows[new_y][dst].parents.push(start_x as usize);

    create_paths_rec(rows, new_x, new_y, rng);
}

/// getCommonAncestor：同层两节点向上最多 max_depth 层找公共祖先。
/// 每层 l 取最右父、r 取最左父（收拢）；返回祖先所在行号。
fn common_ancestor(
    rows: &[Vec<MapNode>],
    node1: (i32, usize),
    node2: (i32, usize),
    max_depth: usize,
) -> Option<usize> {
    debug_assert_eq!(node1.1, node2.1);
    if node1 == node2 {
        return None;
    }
    let (mut l, mut r): ((i32, usize), (i32, usize)) = if node1.0 < node2.0 {
        (node1, node2)
    } else {
        (node2, node1)
    };
    let start_y = node1.1 as i32;
    let mut current_y = start_y;
    while current_y >= 0 && current_y >= start_y - max_depth as i32 {
        let lp = &rows[current_y as usize][l.0 as usize].parents;
        let rp = &rows[current_y as usize][r.0 as usize].parents;
        if lp.is_empty() || rp.is_empty() {
            return None;
        }
        l = (*lp.iter().max()? as i32, (current_y - 1) as usize);
        r = (*rp.iter().min()? as i32, (current_y - 1) as usize);
        if l == r {
            return Some(l.1);
        }
        current_y -= 1;
    }
    None
}

/// filterRedundantEdgesFromRow：首行同目标重边只留一条
fn filter_redundant_edges(rows: &mut [Vec<MapNode>]) {
    for node in rows[0].iter_mut() {
        if node.edges.is_empty() {
            continue;
        }
        let mut seen: Vec<usize> = Vec::new();
        let mut kept: Vec<usize> = Vec::new();
        for &e in &node.edges {
            if !seen.contains(&e) {
                seen.push(e);
                kept.push(e);
            }
        }
        node.edges = kept;
    }
}

/// 幕概率表（Exordium/TheCity/TheBeyond 构造器一致）
struct ActChances {
    shop: f32,
    rest: f32,
    event: f32,
    elite: f32,
}
const ACT_CHANCES: ActChances = ActChances {
    shop: 0.05,
    rest: 0.12,
    event: 0.22,
    elite: 0.08,
};

/// Java Math.round(f) = floor(x + 0.5)
fn jround(x: f32) -> usize {
    (x + 0.5).floor() as usize
}

/// AbstractDungeon.generateMap 房间类型部分 + RoomTypeAssigner 全规则
fn assign_room_types(_act: u8, ascension: i32, rows: &mut [Vec<MapNode>], rng: &mut StsRandom) {
    // 1) 统计待分配节点（有边且非倒数第二行）
    let count = rows
        .iter()
        .flat_map(|r| r.iter())
        .filter(|n| !n.edges.is_empty() && n.y != PRE_BOSS_ROW)
        .count();

    // 2) 类型池（shop/rest/elite/event；A1+ 精英 ×1.6）
    let f = count as f32;
    let mut rooms: Vec<RoomKind> = Vec::new();
    for _ in 0..jround(f * ACT_CHANCES.shop) {
        rooms.push(RoomKind::Shop);
    }
    for _ in 0..jround(f * ACT_CHANCES.rest) {
        rooms.push(RoomKind::Rest);
    }
    let elite_n = if ascension >= 1 {
        jround(f * ACT_CHANCES.elite * 1.6)
    } else {
        jround(f * ACT_CHANCES.elite)
    };
    for _ in 0..elite_n {
        rooms.push(RoomKind::Elite);
    }
    for _ in 0..jround(f * ACT_CHANCES.event) {
        rooms.push(RoomKind::Event);
    }
    // 补战斗到节点数（源码 while 循环）；超出截断
    while rooms.len() < count {
        rooms.push(RoomKind::Monster);
    }
    rooms.truncate(count);

    // 3) 固定行：0=战斗 8=宝箱 14=休息（assignRowAsRoomType 只改未分配节点；
    //    池按全部节点数生成——固定行使部分池项被弃用，与源码一致）
    for row in rows.iter_mut() {
        for n in row.iter_mut() {
            if n.edges.is_empty() {
                continue;
            }
            if n.y == 0 {
                n.room = Some(RoomKind::Monster);
            } else if n.y == 8 {
                n.room = Some(RoomKind::Treasury);
            } else if n.y == MAP_HEIGHT - 1 {
                n.room = Some(RoomKind::Rest);
            }
        }
    }

    // 4) Collections.shuffle(roomList, mapRng)：降序 Fisher-Yates，nextInt(i+1)
    for i in (1..rooms.len()).rev() {
        let j = rng.random_range(i as i32) as usize;
        rooms.swap(i, j);
    }

    // 5) assignRoomsToNodes：行序 × 列序
    for y in 0..rows.len() {
        for x in 0..MAP_WIDTH {
            if rows[y][x].edges.is_empty() || rows[y][x].room.is_some() {
                continue;
            }
            if let Some(idx) = next_room_by_rules(rows, y, x, &rooms) {
                rows[y][x].room = Some(rooms[idx]);
                rooms.remove(idx);
            }
        }
    }

    // 6) lastMinuteNodeChecker：漏网节点补战斗
    for row in rows.iter_mut() {
        for n in row.iter_mut() {
            if !n.edges.is_empty() && n.room.is_none() {
                n.room = Some(RoomKind::Monster);
            }
        }
    }
}

/// getNextRoomTypeAccordingToRules：返回池中首个可放类型的下标
fn next_room_by_rules(
    rows: &[Vec<MapNode>],
    y: usize,
    x: usize,
    rooms: &[RoomKind],
) -> Option<usize> {
    let n = &rows[y][x];
    // 兄弟 = 父节点们的其他子
    let mut siblings: Vec<(usize, usize)> = Vec::new();
    for &px in &n.parents {
        for &e in &rows[y - 1][px].edges {
            if e != n.x {
                siblings.push((y, e));
            }
        }
    }
    for (idx, &candidate) in rooms.iter().enumerate() {
        // ruleAssignableToRow
        if n.y <= 4 && matches!(candidate, RoomKind::Rest | RoomKind::Elite) {
            continue;
        }
        if n.y >= 13 && candidate == RoomKind::Rest {
            continue;
        }
        // ruleParentMatches（适用：休息/宝箱/商店/精英）
        let parent_match = n.parents.iter().any(|&px| {
            matches!(
                candidate,
                RoomKind::Rest | RoomKind::Treasury | RoomKind::Shop | RoomKind::Elite
            ) && rows[y - 1][px].room == Some(candidate)
        });
        // ruleSiblingMatches（适用：休息/战斗/事件/精英/商店）
        let sibling_match = siblings.iter().any(|&(sy, sx)| {
            matches!(
                candidate,
                RoomKind::Rest
                    | RoomKind::Monster
                    | RoomKind::Event
                    | RoomKind::Elite
                    | RoomKind::Shop
            ) && rows[sy][sx].room == Some(candidate)
        });
        if !parent_match && !sibling_match {
            return Some(idx);
        }
        // y==0 直接放（源码 n.y != 0 continue；行 0 已固定，实际不可达）
        if n.y == 0 {
            return Some(idx);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::Action;
    use crate::combat::Q;
    use crate::powers::PowerKind;
    use crate::rng::StsRandom;

    fn edge_sig(m: &GameMap) -> Vec<Vec<Vec<usize>>> {
        m.rows
            .iter()
            .map(|r| r.iter().map(|n| n.edges.clone()).collect())
            .collect()
    }

    #[test]
    fn map_deterministic_same_seed() {
        let a = generate_dungeon(1, 20, &mut StsRandom::new(42));
        let b = generate_dungeon(1, 20, &mut StsRandom::new(42));
        assert_eq!(edge_sig(&a), edge_sig(&b));
        let c = generate_dungeon(1, 20, &mut StsRandom::new(43));
        assert_ne!(edge_sig(&a), edge_sig(&c));
    }

    #[test]
    fn map_structure_invariants() {
        for seed in [1u64, 7, 99, 2024, 88888] {
            let m = generate_dungeon(1, 20, &mut StsRandom::new(seed));
            // 固定行
            for x in 0..MAP_WIDTH {
                let n = &m.rows[0][x];
                if !n.edges.is_empty() {
                    assert_eq!(n.room, Some(RoomKind::Monster), "行0=战斗");
                }
                let n8 = &m.rows[8][x];
                if !n8.edges.is_empty() {
                    assert_eq!(n8.room, Some(RoomKind::Treasury), "行8=宝箱");
                }
                let n14 = &m.rows[14][x];
                if !n14.edges.is_empty() {
                    assert_eq!(n14.room, Some(RoomKind::Rest), "行14=休息");
                }
            }
            // 边指向存在的列；行 0 规则：无休息/精英 ≤4 行、无休息 ≥13 行
            for (y, row) in m.rows.iter().enumerate() {
                for n in row {
                    for &e in &n.edges {
                        assert!(e < MAP_WIDTH);
                    }
                    if let Some(r) = n.room {
                        if y <= 4 {
                            assert!(!matches!(r, RoomKind::Rest | RoomKind::Elite));
                        }
                        if y == 13 {
                            assert_ne!(r, RoomKind::Rest);
                        }
                    }
                }
            }
            // 有 Boss 入口
            assert!(!m.boss_columns().is_empty());
        }
    }

    #[test]
    fn map_paths_reachable_to_boss() {
        for seed in [5u64, 11, 77] {
            let m = generate_dungeon(1, 20, &mut StsRandom::new(seed));
            for n in &m.rows[0] {
                if n.edges.is_empty() {
                    continue;
                }
                let mut cur = n;
                while cur.y < MAP_HEIGHT - 1 {
                    let next_x = cur.edges[0];
                    cur = &m.rows[cur.y + 1][next_x];
                }
                assert!(!cur.edges.is_empty(), "行 14 有 Boss 哨兵边");
            }
        }
    }

    /// 大史莱姆：半血触发分裂 → 自灭 + 2 只 M 版各承当前 HP
    #[test]
    fn large_slime_split() {
        let mut c = Combat::new_with(3, 20, &[MonsterKind::AcidSlimeL]);
        let max = c.monsters[0].creature.max_hp;
        // 打到半血以下 → 定招分裂（3）
        c.exec(Q::DamageMonster {
            target: 0,
            dmg: max / 2 + 1,
        });
        assert!(!c.monsters[0].creature.is_dead(), "未死");
        assert_eq!(c.monsters[0].next_move, 3, "半血定招分裂");
        let hp_at_split = c.monsters[0].creature.hp;
        // 分裂回合
        c.step(&Action::EndTurn);
        // L 版自灭 + 2 只 M 版入場
        assert!(c.monsters[0].creature.is_dead(), "L 版自灭");
        assert_eq!(c.monsters.len(), 3, "2 只 M 版");
        assert_eq!(
            c.monsters[1].creature.max_hp, hp_at_split,
            "M 版承分裂时 HP"
        );
        assert_eq!(c.monsters[2].creature.max_hp, hp_at_split);
        // 战斗继续（M 版可打）
        assert!(!c.over);
    }

    /// 大史莱姆战利：粘液冲撞；脆舔/弱舔
    #[test]
    fn large_slime_moves() {
        // 酸：腐蚀冲撞 12(A2+) + 2 粘液
        let mut c = Combat::new_with(5, 20, &[MonsterKind::AcidSlimeL]);
        c.monsters[0].next_move = 1;
        let hp0 = c.player.creature.hp;
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        let p0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, p0 - 12);
        let slimed = c
            .player
            .discard
            .iter()
            .filter(|x| x.def.id == "Slimed")
            .count();
        assert_eq!(slimed, 2, "腐蚀冲撞+2 粘液");
        let _ = hp0;

        // 尖：火焰冲撞 18(A2+) + 2 粘液；脆舔 2(A17+ 3)
        let mut c = Combat::new_with(6, 20, &[MonsterKind::SpikeSlimeL]);
        c.player.creature.hp = 999;
        c.player.creature.max_hp = 999;
        c.monsters[0].next_move = 1;
        let p0 = c.player.creature.hp;
        c.step(&Action::EndTurn);
        assert_eq!(c.player.creature.hp, p0 - 18);
        c.monsters[0].next_move = 4;
        c.monsters[0].move_history.push(4);
        c.step(&Action::EndTurn);
        assert!(c
            .player
            .creature
            .powers
            .iter()
            .any(|p| p.kind == PowerKind::Frail));
    }

    /// Act 1 池：Large/Lots of Slimes 已恢复
    #[test]
    fn act1_pool_complete() {
        let (weak, strong, _elite) = act1_pools();
        assert_eq!(weak.len(), 4);
        assert!(strong.iter().any(|e| e.name == "Large Slime"));
        assert!(strong.iter().any(|e| e.name == "Lots of Slimes"));
        assert!(strong.len() >= 10, "强怪池 10+ 项（含恢复的史莱姆）");
    }

    /// Boss 战利 + 幕间卡牌：Act1→2 转幕
    #[test]
    fn boss_relic_and_interlude() {
        let mut run = RunState::new(19, 20);
        run.pos = (14, 3);
        run.map.rows[14][3].edges = vec![3];
        run.map.rows[14][3].room = Some(RoomKind::Rest);
        // 直接 Boss 战
        run.combat = Some(Combat::new_run_battle(
            99,
            20,
            run.boss,
            run.relics.clone(),
            run.deck.clone(),
            run.potions.clone(),
            999,
            999,
            99,
        ));
        if let Some(c) = run.combat.as_mut() {
            for m in c.monsters.iter_mut() {
                m.creature.hp = 0;
            }
            c.over = true;
            c.won = true;
        }
        let relics_before = run.relics.len();
        let deck_before = run.deck.len();
        run.settle_combat(0);
        assert_eq!(run.act, 2, "转幕");
        assert!(run.relics.len() > relics_before, "Boss 战利入栏");
        assert!(run.deck.len() > deck_before, "幕间卡牌入组");
    }

    /// 奖励管线：金币区间/药水怜悯/暴雪偏移/卡牌去重/稀有度
    #[test]
    fn rewards_pipeline() {
        let mut run = RunState::new(11, 20);
        run.map.rows[1][3].edges = vec![3]; // 确保有边（视为可达节点）
        run.map.rows[1][3].room = Some(RoomKind::Monster);
        run.pos = (1, 3);
        // 模拟战斗胜利（注入局外遗物，settle 会回写）
        run.combat = Some(Combat::new_run_battle(
            1,
            20,
            &[MonsterKind::JawWorm],
            run.relics.clone(),
            run.deck.clone(),
            run.potions.clone(),
            80,
            80,
            99,
        ));
        if let Some(c) = run.combat.as_mut() {
            for m in c.monsters.iter_mut() {
                m.creature.hp = 0;
            }
            c.over = true;
            c.won = true;
        }
        run.settle_combat(0);
        // 奖励：金币 + 药水(概率) + 3 张卡
        assert!(run.rewards.len() >= 4, "金币+3卡至少 4 项");
        let golds: Vec<_> = run
            .rewards
            .iter()
            .filter(|r| matches!(r, RewardItem::Gold(g) if (10..=20).contains(g)))
            .collect();
        assert_eq!(golds.len(), 1, "金币 10-20");
        let cards: Vec<_> = run
            .rewards
            .iter()
            .filter(|r| matches!(r, RewardItem::Card(_)))
            .collect();
        assert_eq!(cards.len(), 3, "3 张卡");
        // 卡牌去重
        if let [RewardItem::Card(a), RewardItem::Card(b), RewardItem::Card(c)] =
            &run.rewards[run.rewards.len() - 3..]
        {
            assert!(a.def.id != b.def.id && b.def.id != c.def.id && a.def.id != c.def.id);
        }
        // 领取：金币 + 一张卡
        let card_idx = run
            .rewards
            .iter()
            .position(|r| matches!(r, RewardItem::Card(_)))
            .unwrap();
        let deck_before = run.deck.len();
        run.take_reward(card_idx);
        assert_eq!(run.deck.len(), deck_before + 1, "卡进主牌组");
        run.take_all_rewards_skip_cards();
        assert!(run.player.gold >= 99 + 10, "金币累计");
    }

    /// 精英奖励：遗物 + 金币 25-35；怜悯递增
    #[test]
    fn elite_rewards_relic() {
        let mut run = RunState::new(13, 20);
        run.pos = (5, 2);
        run.map.rows[5][2].room = Some(RoomKind::Elite);
        run.combat = Some(Combat::new_run_battle(
            2,
            20,
            &[MonsterKind::GremlinNob],
            run.relics.clone(),
            run.deck.clone(),
            run.potions.clone(),
            999,
            999,
            99,
        ));
        if let Some(c) = run.combat.as_mut() {
            for m in c.monsters.iter_mut() {
                m.creature.hp = 0;
            }
            c.over = true;
            c.won = true;
        }
        run.settle_combat(0);
        let has_relic = run
            .rewards
            .iter()
            .any(|r| matches!(r, RewardItem::Relic(_)));
        assert!(has_relic, "精英掉遗物");
        let gold_ok = run
            .rewards
            .iter()
            .any(|r| matches!(r, RewardItem::Gold(g) if (25..=35).contains(g)));
        assert!(gold_ok, "精英金币 25-35");
        run.take_all_rewards_skip_cards();
        assert!(run.relics.len() >= 2, "遗物入栏（含起始燃烧之血）");
    }

    /// 整局带牌组/药水/遗物流转：战内使用药水→局外同步；主牌组进战斗
    #[test]
    fn run_carries_deck_potions_relics() {
        let mut run = RunState::new(17, 20);
        // 拿一张打击进主牌组
        run.deck.push(crate::cards::CardInst::new("Strike_R"));
        let deck_n = run.deck.len();
        run.relics.push(crate::relics::RelicInst::new(
            crate::relics::RelicKind::Vajra,
        ));
        run.potions[0] = Some(crate::potions::PotionInst::new(
            crate::potions::PotionKind::Fire,
        ));
        run.pos = (1, 2);
        run.enter_room();
        let c = run.combat.as_ref().unwrap();
        // 战斗注入：抽牌堆 = 主牌组（洗后）+遗物生效
        assert_eq!(
            c.player.draw.len() + c.player.hand.len(),
            deck_n,
            "主牌组进战斗"
        );
        assert!(c
            .player_relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::Vajra));
        // 战内喝药水
        let mut c = run.combat.take().unwrap();
        c.step(&crate::combat::Action::UsePotion {
            index: 0,
            target: Some(0),
        });
        assert!(c.player.potions[0].is_none(), "战内喝掉");
        run.combat = Some(c);
        run.settle_combat(0);
        assert!(run.potions[0].is_none(), "局外同步");
    }

    /// RunState 冒烟：Act 2 全流程（战斗/休息/宝箱占位/Boss→转幕→胜利）
    #[test]
    fn run_state_smoke_act2() {
        let mut run = RunState::new(7, 20);
        // 跳到 Act 2（直接重建）
        run.act = 2;
        run.map = generate_dungeon(2, 20, &mut run.rng.map);
        run.pos = (0, 0);
        run.populate_act();
        run.player.hp = 9999;
        run.player.max_hp = 9999;
        assert!(!run.encounter_list.is_empty());
        assert!(!run.elite_list.is_empty());
        assert!(!run.boss.is_empty());
        // 沿第一条边走完 0..14 行，每层进房结算
        for _ in 0..MAP_HEIGHT {
            run.choose_node(0);
            run.enter_room();
            if let Some(c) = run.combat.as_mut() {
                for m in c.monsters.iter_mut() {
                    m.creature.hp = 0;
                }
                c.over = true;
                c.won = true;
            }
            run.settle_combat(10);
        }
        assert_eq!(run.pos.0, MAP_HEIGHT - 1, "到达行 14");
        // 行 14 → Boss 战（简化路径：直接 boss 战）
        let boss = run.boss;
        let mut c = crate::combat::Combat::new_with(run.seed, run.ascension, boss);
        c.player.creature.hp = run.player.hp;
        c.player.creature.max_hp = run.player.max_hp;
        c.gold = run.player.gold;
        for m in c.monsters.iter_mut() {
            m.creature.hp = 0;
        }
        c.over = true;
        c.won = true;
        run.combat = Some(c);
        run.settle_combat(100);
        assert_eq!(run.act, 3, "Act 2 Boss 胜 → Act 3");
        assert!(!run.over);
        assert!(run.floor >= (MAP_HEIGHT - 1) as u32, "走了 14 层");
        assert!(run.player.gold > 99, "战斗金币累计");
    }

    /// 遭遇序列：防重复规则 + 权重 roll 确定性
    #[test]
    fn encounter_list_deterministic() {
        let a = RunState::new(42, 20);
        let b = RunState::new(42, 20);
        assert_eq!(
            a.encounter_list.len(),
            b.encounter_list.len(),
            "弱 2 + 强 12 = 14"
        );
        assert_eq!(a.encounter_list, b.encounter_list);
        assert_eq!(a.elite_list, b.elite_list);
        let c = RunState::new(43, 20);
        assert_ne!(a.encounter_list, c.encounter_list);
    }

    #[test]
    fn map_room_counts_reasonable() {
        // A20：精英 ~ round(count×0.08×1.6)；商店/休息/事件按概率
        let m = generate_dungeon(1, 20, &mut StsRandom::new(3));
        let mut elites = 0;
        let mut shops = 0;
        let mut rests = 0;
        let mut events = 0;
        for row in &m.rows {
            for n in row {
                match n.room {
                    Some(RoomKind::Elite) => elites += 1,
                    Some(RoomKind::Shop) => shops += 1,
                    Some(RoomKind::Rest) => rests += 1,
                    Some(RoomKind::Event) => events += 1,
                    _ => {}
                }
            }
        }
        assert!((2..=8).contains(&elites), "精英数 {elites}");
        assert!((1..=4).contains(&shops), "商店数 {shops}");
        assert!(rests >= 2, "休息数 {rests}（含行14）");
        assert!(events >= 3, "事件数 {events}");
    }
}

// ═══════════════ 遭遇池（MonsterHelper + 各幕 generateXxxEnemies）═══════════════

use crate::state::MonsterKind;

/// MonsterInfo：名字 + 权重（roll 前归一化）
pub struct EncInfo {
    pub name: &'static str,
    pub weight: f32,
    /// 遭遇组合（按出场序）
    pub kinds: &'static [MonsterKind],
}

/// MonsterInfo.roll：累积权重命中（roll ∈ [0,1)）
pub fn enc_roll(list: &[EncInfo], roll: f32) -> &'static str {
    let total: f32 = list.iter().map(|e| e.weight).sum();
    let mut cur = 0.0f32;
    for e in list {
        cur += e.weight / total;
        if roll < cur {
            return e.name;
        }
    }
    list.last().map(|e| e.name).unwrap_or("ERROR")
}

pub fn enc_kinds(list: &[EncInfo], name: &str) -> &'static [MonsterKind] {
    list.iter()
        .find(|e| e.name == name)
        .map(|e| e.kinds)
        .unwrap_or(&[])
}

/// Act 1 遭遇池（Exordium）——全量可组。
/// ⚠️ 残留偏差：Large Slime 的酸/尖二选一走源码 slimeRng（此处以 acid 表记）；
/// Lots of Slimes 的 5 小怪混排走 miscRng（此处固定 3尖+2酸组合表记）
pub fn act1_pools() -> (Vec<EncInfo>, Vec<EncInfo>, Vec<EncInfo>) {
    use MonsterKind::*;
    let weak = vec![
        EncInfo {
            name: "Cultist",
            weight: 2.0,
            kinds: &[Cultist],
        },
        EncInfo {
            name: "Jaw Worm",
            weight: 2.0,
            kinds: &[JawWorm],
        },
        EncInfo {
            name: "2 Louse",
            weight: 2.0,
            kinds: &[LouseNormal, LouseDefensive],
        },
        EncInfo {
            name: "Small Slimes",
            weight: 2.0,
            kinds: &[AcidSlimeS, SpikeSlimeS],
        },
    ];
    let strong = vec![
        EncInfo {
            name: "Blue Slaver",
            weight: 2.0,
            kinds: &[SlaverBlue],
        },
        EncInfo {
            name: "Gremlin Gang",
            weight: 1.0,
            kinds: &[GremlinThief, GremlinWarrior, GremlinFat],
        },
        EncInfo {
            name: "Looter",
            weight: 2.0,
            kinds: &[Looter],
        },
        EncInfo {
            name: "Large Slime",
            weight: 2.0,
            kinds: &[AcidSlimeL], // 酸/尖各半（源码 slimeRng；此处以 acid 表记，偏差登记）
        },
        EncInfo {
            name: "Lots of Slimes",
            weight: 1.0,
            kinds: &[
                SpikeSlimeS,
                AcidSlimeS,
                SpikeSlimeS,
                AcidSlimeS,
                SpikeSlimeS,
            ],
        },
        EncInfo {
            name: "Exordium Thugs",
            weight: 1.5,
            kinds: &[Looter, Looter, SlaverBlue],
        },
        EncInfo {
            name: "Exordium Wildlife",
            weight: 1.5,
            kinds: &[FungiBeast, LouseNormal],
        },
        EncInfo {
            name: "Red Slaver",
            weight: 1.0,
            kinds: &[SlaverRed],
        },
        EncInfo {
            name: "3 Louse",
            weight: 2.0,
            kinds: &[LouseNormal, LouseDefensive, LouseNormal],
        },
        EncInfo {
            name: "2 Fungi Beasts",
            weight: 2.0,
            kinds: &[FungiBeast, FungiBeast],
        },
    ];
    let elites = vec![
        EncInfo {
            name: "Gremlin Nob",
            weight: 1.0,
            kinds: &[GremlinNob],
        },
        EncInfo {
            name: "Lagavulin",
            weight: 1.0,
            kinds: &[Lagavulin],
        },
        EncInfo {
            name: "3 Sentries",
            weight: 1.0,
            kinds: &[Sentry, Sentry, Sentry],
        },
    ];
    (weak, strong, elites)
}

pub fn act1_bosses() -> Vec<EncInfo> {
    use MonsterKind::*;
    vec![
        EncInfo {
            name: "The Guardian",
            weight: 1.0,
            kinds: &[Guardian],
        },
        EncInfo {
            name: "Slime Boss",
            weight: 1.0,
            kinds: &[SlimeBoss],
        },
        EncInfo {
            name: "Hexaghost",
            weight: 1.0,
            kinds: &[Hexaghost],
        },
    ]
}

/// Act 2 遭遇池（TheCity）——组合全量可组
pub fn act2_pools() -> (Vec<EncInfo>, Vec<EncInfo>, Vec<EncInfo>) {
    use MonsterKind::*;
    let weak = vec![
        EncInfo {
            name: "Spheric Guardian",
            weight: 2.0,
            kinds: &[SphericGuardian],
        },
        EncInfo {
            name: "Chosen",
            weight: 2.0,
            kinds: &[Chosen],
        },
        EncInfo {
            name: "Shell Parasite",
            weight: 2.0,
            kinds: &[ShelledParasite],
        },
        EncInfo {
            name: "3 Byrds",
            weight: 2.0,
            kinds: &[Byrd, Byrd, Byrd],
        },
        EncInfo {
            name: "2 Thieves",
            weight: 2.0,
            kinds: &[Mugger, Mugger],
        },
    ];
    let strong = vec![
        EncInfo {
            name: "Chosen and Byrds",
            weight: 2.0,
            kinds: &[Byrd, Chosen],
        },
        EncInfo {
            name: "Sentry and Sphere",
            weight: 2.0,
            kinds: &[Sentry, SphericGuardian],
        },
        EncInfo {
            name: "Snake Plant",
            weight: 6.0,
            kinds: &[SnakePlant],
        },
        EncInfo {
            name: "Snecko",
            weight: 4.0,
            kinds: &[Snecko],
        },
        EncInfo {
            name: "Centurion and Healer",
            weight: 6.0,
            kinds: &[Centurion, Healer],
        },
        EncInfo {
            name: "Cultist and Chosen",
            weight: 3.0,
            kinds: &[Cultist, Chosen],
        },
        EncInfo {
            name: "3 Cultists",
            weight: 3.0,
            kinds: &[Cultist, Cultist, Cultist],
        },
        EncInfo {
            name: "Shelled Parasite and Fungi",
            weight: 3.0,
            kinds: &[ShelledParasite, FungiBeast],
        },
    ];
    let elites = vec![
        EncInfo {
            name: "Gremlin Leader",
            weight: 1.0,
            kinds: &[GremlinThief, GremlinFat, GremlinLeader],
        },
        EncInfo {
            name: "Slavers",
            weight: 1.0,
            kinds: &[SlaverBlue, SlaverRed, Taskmaster],
        },
        EncInfo {
            name: "Book of Stabbing",
            weight: 1.0,
            kinds: &[BookOfStabbing],
        },
    ];
    (weak, strong, elites)
}

pub fn act2_bosses() -> Vec<EncInfo> {
    use MonsterKind::*;
    vec![
        EncInfo {
            name: "Bronze Automaton",
            weight: 1.0,
            kinds: &[BronzeAutomaton],
        },
        EncInfo {
            name: "The Collector",
            weight: 1.0,
            kinds: &[TheCollector],
        },
        EncInfo {
            name: "Champ",
            weight: 1.0,
            kinds: &[Champ],
        },
    ]
}

/// Act 3 遭遇池（TheBeyond）——组合全量可组
pub fn act3_pools() -> (Vec<EncInfo>, Vec<EncInfo>, Vec<EncInfo>) {
    use MonsterKind::*;
    let weak = vec![
        EncInfo {
            name: "3 Darklings",
            weight: 2.0,
            kinds: &[Darkling, Darkling, Darkling],
        },
        EncInfo {
            name: "Orb Walker",
            weight: 2.0,
            kinds: &[OrbWalker],
        },
        EncInfo {
            name: "3 Shapes",
            weight: 2.0,
            kinds: &[Repulsor, Repulsor, Repulsor],
        },
    ];
    let strong = vec![
        EncInfo {
            name: "Spire Growth",
            weight: 1.0,
            kinds: &[SpireGrowth],
        },
        EncInfo {
            name: "Transient",
            weight: 1.0,
            kinds: &[Transient],
        },
        EncInfo {
            name: "4 Shapes",
            weight: 1.0,
            kinds: &[Repulsor, Repulsor, Repulsor, Repulsor],
        },
        EncInfo {
            name: "Maw",
            weight: 1.0,
            kinds: &[Maw],
        },
        EncInfo {
            name: "Sphere and 2 Shapes",
            weight: 1.0,
            kinds: &[SphericGuardian, Repulsor, Repulsor],
        },
        EncInfo {
            name: "Jaw Worm Horde",
            weight: 1.0,
            kinds: &[JawWorm, JawWorm, JawWorm],
        },
        EncInfo {
            name: "Writhing Mass",
            weight: 1.0,
            kinds: &[WrithingMass],
        },
    ];
    let elites = vec![
        EncInfo {
            name: "Giant Head",
            weight: 2.0,
            kinds: &[GiantHead],
        },
        EncInfo {
            name: "Nemesis",
            weight: 2.0,
            kinds: &[Nemesis],
        },
        EncInfo {
            name: "Reptomancer",
            weight: 2.0,
            kinds: &[Reptomancer],
        },
    ];
    (weak, strong, elites)
}

pub fn act3_bosses() -> Vec<EncInfo> {
    use MonsterKind::*;
    vec![
        EncInfo {
            name: "Awakened One",
            weight: 1.0,
            kinds: &[AwakenedOne],
        },
        EncInfo {
            name: "Donu and Deca",
            weight: 1.0,
            kinds: &[Deca, Donu],
        },
        EncInfo {
            name: "Time Eater",
            weight: 1.0,
            kinds: &[TimeEater],
        },
    ]
}

// ═══════════════ RunState：整局状态机骨架 ═══════════════

use crate::combat::Combat;
use crate::rng::RngStreams;

/// 局内玩家元状态（局外层）
#[derive(Clone, Debug)]
pub struct RunPlayer {
    pub hp: i32,
    pub max_hp: i32,
    pub gold: i32,
}

/// 奖励类型（RewardItem.RewardType 子集）
#[derive(Clone, Debug)]
pub enum RewardItem {
    Gold(i32),
    Card(crate::cards::CardInst),
    Potion(crate::potions::PotionKind),
    Relic(crate::relics::RelicKind),
}

/// 整局状态：幕/层/遭遇序列/当前战斗
pub struct RunState {
    pub seed: u64,
    pub ascension: i32,
    pub act: u8,
    pub map: GameMap,
    pub pos: (usize, usize), // 当前节点 (y, x)
    pub player: RunPlayer,
    pub rng: RngStreams,
    /// 幕内遭遇序列（populateMonsterList 产物：前 1-2 个弱怪，其余强怪）
    pub encounter_list: Vec<&'static [MonsterKind]>,
    pub encounter_idx: usize,
    pub elite_list: Vec<&'static [MonsterKind]>,
    pub elite_idx: usize,
    pub boss: &'static [MonsterKind],
    pub combat: Option<Combat>,
    pub floor: u32,
    pub over: bool,
    pub won: bool,
    /// 主牌组（局外层权威，进战斗复制洗牌）
    pub deck: Vec<crate::cards::CardInst>,
    /// 药水栏 3 槽
    pub potions: Vec<Option<crate::potions::PotionInst>>,
    /// 已获遗物（实例带跨战计数）
    pub relics: Vec<crate::relics::RelicInst>,
    /// 暴雪稀有度偏移（起 5，普通-1 至 -40，稀有重置 5）
    card_blizz: i32,
    /// 药水怜悯（掉 40% 未中 +10，中 -10）
    potion_pity: i32,
    /// 当前房间奖励（待领取）
    pub rewards: Vec<RewardItem>,
    /// 房间结束后待结算：拿走的卡进主牌组等
    pub pending_relic_tier_roll: bool,
}

impl RunState {
    /// 开新局：生成 Act 1 地图 + 遭遇序列（populateMonsterList 语义：
    /// 弱怪 2 / 强怪 12 / 精英 10；首强怪排除表；不与上一个/上上个重复）
    pub fn new(seed: u64, ascension: i32) -> Self {
        let mut rng = RngStreams::new(seed);
        let map = generate_dungeon(1, ascension, &mut rng.map);
        let mut s = Self {
            seed,
            ascension,
            act: 1,
            map,
            pos: (0, 0),
            player: RunPlayer {
                hp: if ascension >= 6 { 72 } else { 80 },
                max_hp: 80,
                gold: 99,
            },
            rng,
            encounter_list: Vec::new(),
            encounter_idx: 0,
            elite_list: Vec::new(),
            elite_idx: 0,
            boss: &[],
            combat: None,
            floor: 0,
            over: false,
            won: false,
            deck: crate::cards::ironclad_starter(ascension),
            potions: vec![None, None, None],
            relics: vec![crate::relics::RelicInst::new(
                crate::relics::RelicKind::BurningBlood,
            )],
            card_blizz: 5,
            potion_pity: 0,
            rewards: Vec::new(),
            pending_relic_tier_roll: false,
        };
        s.populate_act();
        s
    }

    fn act_pools(&self) -> (Vec<EncInfo>, Vec<EncInfo>, Vec<EncInfo>, Vec<EncInfo>) {
        match self.act {
            1 => {
                let (w, s, e) = act1_pools();
                (w, s, e, act1_bosses())
            }
            2 => {
                let (w, s, e) = act2_pools();
                (w, s, e, act2_bosses())
            }
            _ => {
                let (w, s, e) = act3_pools();
                (w, s, e, act3_bosses())
            }
        }
    }

    /// populateMonsterList：弱怪 2；强怪 12（含首怪排除表）；精英 10；Boss 洗牌取 1
    fn populate_act(&mut self) {
        let (weak, strong, elites, bosses) = self.act_pools();
        // 弱怪 2（相邻不重复由列表语义：前 2 个弱怪可以相同？源码同样走防重逻辑）
        let mut list: Vec<&'static str> = Vec::new();
        while list.len() < 2 {
            let name = enc_roll(&weak, self.rng.monster.random_float());
            if list.last() != Some(&name) || list.is_empty() {
                if !list.is_empty() && name == list[list.len() - 1] {
                    continue; // 与上一个相同 → 重掷
                }
                list.push(name);
            }
        }
        // 强怪 12：防上/上上重复
        while list.len() < 14 {
            let name = enc_roll(&strong, self.rng.monster.random_float());
            let n = list.len();
            if n >= 1 && name == list[n - 1] {
                continue;
            }
            if n >= 2 && name == list[n - 2] {
                continue;
            }
            list.push(name);
        }
        self.encounter_list = list
            .iter()
            .map(|&n| enc_kinds(&weak, n).if_empty(enc_kinds(&strong, n)))
            .collect();
        // 精英 10（相邻不重复）
        let mut elist: Vec<&'static str> = Vec::new();
        while elist.len() < 10 {
            let name = enc_roll(&elites, self.rng.monster.random_float());
            if !elist.is_empty() && name == elist[elist.len() - 1] {
                continue;
            }
            elist.push(name);
        }
        self.elite_list = elist.iter().map(|&n| enc_kinds(&elites, n)).collect();
        // Boss：Collections.shuffle(bossList, new Random(monsterRng.randomLong())) 取 0
        let boss_name = bosses[0].name;
        self.boss = enc_kinds(&bosses, boss_name);
        // TODO(M3 后续)：Boss 洗牌按源码（monsterRng.randomLong 种子化 JavaRandom）
    }

    /// 选择下一节点（行动空间 = 当前节点边）。
    /// 行 14 的边是 Boss 哨兵：不再前进，位置保持（Boss 房由 enter_room 进入）
    pub fn choose_node(&mut self, edge_idx: usize) {
        let (y, x) = self.pos;
        if y + 1 >= MAP_HEIGHT {
            return;
        }
        let next_x = self.map.rows[y][x].edges[edge_idx];
        self.pos = (y + 1, next_x);
        self.floor += 1;
    }

    /// 进入当前节点的房间。战斗类经 `Combat::new_run_battle` 注入主牌组/药水栏/
    /// 遗物栏/局外 HP 与金币；休息房回 30%；事件/商店/宝箱为占位（下一批）
    pub fn enter_room(&mut self) {
        let (y, x) = self.pos;
        let room = self.map.rows[y][x].room;
        self.rewards.clear();
        match room {
            Some(RoomKind::Monster) | Some(RoomKind::Elite) => {
                let kinds = if room == Some(RoomKind::Elite) {
                    let k = self.elite_list[self.elite_idx];
                    self.elite_idx += 1;
                    k
                } else {
                    let k =
                        self.encounter_list[self.encounter_idx.min(self.encounter_list.len() - 1)];
                    self.encounter_idx += 1;
                    k
                };
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
                // 战斗内喝掉的药水同步回局外
            }
            Some(RoomKind::Rest) => {
                let heal = (self.player.max_hp as f32 * 0.3) as i32;
                self.player.hp = (self.player.hp + heal).min(self.player.max_hp);
            }
            Some(RoomKind::Boss) => {
                let relics: Vec<_> = self.relics.clone();
                self.combat = Some(Combat::new_run_battle(
                    self.seed ^ 0xDEAD_BEEF,
                    self.ascension,
                    self.boss,
                    relics,
                    self.deck.clone(),
                    self.potions.clone(),
                    self.player.hp,
                    self.player.max_hp,
                    self.player.gold,
                ));
            }
            _ => {}
        }
    }

    /// 战斗胜利后生成奖励（AbstractRoom.applyEvents → addGoldToRewards/
    /// dropReward/addPotionToRewards/addCardToRewards 逐行对照）
    pub fn generate_rewards(&mut self) {
        let (y, _) = self.pos;
        let room = self.map.rows[y][0].room.or(Some(RoomKind::Monster));
        let _ = room;
        let room = self.map.rows[y][self.pos.1].room;
        // 1) 金币：普通 treasureRng.random(10,20)；精英 (25,35)
        match room {
            Some(RoomKind::Elite) => {
                let g = self.rng.treasure.random_start_end(25, 35);
                self.rewards.push(RewardItem::Gold(g));
                // 3) 精英掉遗物：relicRng(0,99) <50 普通 >85 稀有 否则罕见
                let roll = self.rng.relic.random_start_end(0, 99);
                let _tier = if roll < 50 {
                    "COMMON"
                } else if roll > 85 {
                    "RARE"
                } else {
                    "UNCOMMON"
                };
                // ⚠️ 偏差：已实现遗物池无 tier 元数据，从 50 只里随机取未持有者
                let owned: Vec<_> = self.relics.iter().map(|r| r.kind).collect();
                let pool: Vec<_> = Self::IMPLEMENTED_RELICS
                    .iter()
                    .copied()
                    .filter(|k| !owned.contains(k))
                    .collect();
                if let Some(&k) =
                    pool.get((self.rng.relic.random_range(pool.len() as i32 - 1).max(0)) as usize)
                {
                    self.rewards.push(RewardItem::Relic(k));
                }
            }
            Some(RoomKind::Monster) => {
                let g = self.rng.treasure.random_start_end(10, 20);
                self.rewards.push(RewardItem::Gold(g));
            }
            _ => {}
        }
        // 2) 药水：40% + pity（掉-10 / 未掉+10）；奖励≥4 项则 0
        let chance = if self.rewards.len() >= 3 {
            0
        } else {
            (40 + self.potion_pity).clamp(0, 100)
        };
        if self.rng.potion.random_start_end(0, 99) < chance {
            let pool = crate::potions::pool_for_class("Ironclad");
            let k = crate::potions::return_random_potion(&pool, false, &mut self.rng.potion);
            self.rewards.push(RewardItem::Potion(k));
            self.potion_pity -= 10;
        } else {
            self.potion_pity += 10;
        }
        // 4) 卡牌奖励：3 张（Boss 恒稀有）
        let boss = room == Some(RoomKind::Boss);
        let mut cards: Vec<crate::cards::CardInst> = Vec::new();
        for _ in 0..3 {
            let rarity = self.roll_rarity(boss);
            let card = self.pick_card(rarity, &cards);
            cards.push(card);
        }
        self.rewards.extend(cards.into_iter().map(RewardItem::Card));
    }

    /// rollRarity：cardRng.random(99) + 暴雪偏移；<3 稀有 / <37 罕见 / 普通
    /// （暴雪：起 5，普通 -1 至 -40，罕见不变，稀有重置 5）
    fn roll_rarity(&mut self, boss: bool) -> &'static str {
        if boss {
            return "RARE";
        }
        let roll = self.rng.card.random_range(99) + self.card_blizz;
        if roll < 3 {
            self.card_blizz = 5;
            "RARE"
        } else if roll < 37 {
            "UNCOMMON"
        } else {
            self.card_blizz = (self.card_blizz - 1).max(-40);
            "COMMON"
        }
    }

    /// getCard：按稀有度从铁甲色池均匀抽取（cardRng.random(size-1)），去重重掷
    fn pick_card(
        &mut self,
        rarity: &'static str,
        already: &[crate::cards::CardInst],
    ) -> crate::cards::CardInst {
        loop {
            let pool: Vec<&'static crate::effects::CardDef> = crate::generated::CARDS
                .iter()
                .filter(|d| d.color == "RED" && d.rarity == rarity && d.status != "no_use")
                .collect();
            if pool.is_empty() {
                // 兜底：任何稀有度（防数据空洞）
                return crate::cards::CardInst::new("Strike_R");
            }
            let roll = self.rng.card.random_range(pool.len() as i32 - 1);
            let def = pool[(roll.max(0) as usize).min(pool.len() - 1)];
            if already.iter().any(|c| c.def.id == def.id) {
                continue; // dupe → 重掷（消耗 cardRng，对照源码 while(containsDupe)）
            }
            return crate::cards::CardInst {
                def,
                upgraded: false,
                damage_bonus: 0,
                block_bonus: 0,
                cost_delta: 0,
            };
        }
    }

    /// 领取奖励：金币即时；卡入主牌组；药水进空槽（满则弃）；遗物入栏
    pub fn take_reward(&mut self, idx: usize) {
        if idx >= self.rewards.len() {
            return;
        }
        let item = self.rewards.remove(idx);
        match item {
            RewardItem::Gold(g) => self.player.gold += g,
            RewardItem::Card(c) => self.deck.push(c),
            RewardItem::Potion(k) => {
                if let Some(slot) = self.potions.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(crate::potions::PotionInst::new(k));
                } // 满槽：弃（简化：满槽领取前应由调用方先 discard）
            }
            RewardItem::Relic(k) => self.relics.push(crate::relics::RelicInst::new(k)),
        }
    }

    /// 领取全部奖励（金币+遗物+药水，跳过全部卡）——机器人策略用
    pub fn take_all_rewards_skip_cards(&mut self) {
        let mut i = 0;
        while i < self.rewards.len() {
            if matches!(self.rewards[i], RewardItem::Card(_)) {
                i += 1;
                continue;
            }
            self.take_reward(i);
        }
        self.rewards.clear();
    }

    /// Boss 战利：从遗物池 roll 3 候选后发放第一只（决策面属协议层）
    fn boss_relic_reward(&mut self) {
        let owned: Vec<_> = self.relics.iter().map(|r| r.kind).collect();
        let pool: Vec<_> = Self::IMPLEMENTED_RELICS
            .iter()
            .copied()
            .filter(|k| !owned.contains(k))
            .collect();
        if pool.is_empty() {
            return;
        }
        let roll = self.rng.relic.random_range(pool.len() as i32 - 1).max(0);
        let k = pool[roll as usize];
        self.relics.push(crate::relics::RelicInst::new(k));
    }

    /// 已实现遗物池（奖励发放用；RelicKind 全集的子集——偏差：无 tier 元数据）
    pub const IMPLEMENTED_RELICS: &[crate::relics::RelicKind] = &[
        crate::relics::RelicKind::Vajra,
        crate::relics::RelicKind::BronzeScales,
        crate::relics::RelicKind::BloodVial,
        crate::relics::RelicKind::Anchor,
        crate::relics::RelicKind::BagOfMarbles,
        crate::relics::RelicKind::OddlySmoothStone,
        crate::relics::RelicKind::Akabeko,
        crate::relics::RelicKind::Shuriken,
        crate::relics::RelicKind::Kunai,
        crate::relics::RelicKind::OrnamentalFan,
        crate::relics::RelicKind::Lantern,
        crate::relics::RelicKind::TungstenRod,
        crate::relics::RelicKind::Torii,
        crate::relics::RelicKind::HappyFlower,
        crate::relics::RelicKind::Sundial,
        crate::relics::RelicKind::ArtOfWar,
        crate::relics::RelicKind::MercuryHourglass,
        crate::relics::RelicKind::StoneCalendar,
    ];

    /// 战斗结束回收：HP/金币回写、药水栏/主牌组增量同步、遗物跨战计数保留、
    /// 胜利生成奖励（金币由 generate_rewards 掷，本方法不再加 gold_reward）
    pub fn settle_combat(&mut self, gold_reward: i32) {
        if let Some(mut c) = self.combat.take() {
            // 胜利遗物钩子（BurningBlood 回血 / FaceOfCleric MaxHP+1）
            let max_hp_gain = if c.won { c.on_victory() } else { 0 };
            self.player.hp = c.player.creature.hp.max(0);
            self.player.max_hp += max_hp_gain;
            // 药水栏同步（战斗内使用/丢弃）
            self.potions = c.player.potions.clone();
            // 主牌组永久新增（OrbWalker 灼伤 / 寄生前置）
            self.deck.extend(c.master_deck_additions.clone());
            // 遗物实例回写（PenNib 跨战计数等）
            self.relics = c.player_relics.clone();
            self.player.gold = c.gold + if c.won { gold_reward } else { 0 };
            if c.won {
                self.generate_rewards();
            }
            if c.won && self.pos.0 == MAP_HEIGHT - 1 {
                // 幕 Boss 战胜利 → 下一幕（本批：3 幕打完即整局胜利）
                if self.act >= 3 {
                    self.over = true;
                    self.won = true;
                } else {
                    // Boss 战利（简化：roll 后直接发放——三选一决策面属协议层）
                    self.boss_relic_reward();
                    // 幕间卡牌奖励（Boss 恒稀有）
                    let mut cards = Vec::new();
                    for _ in 0..3 {
                        let rarity = self.roll_rarity(true);
                        let card = self.pick_card(rarity, &cards);
                        cards.push(card);
                    }
                    self.deck.extend(cards);
                    self.act += 1;
                    self.map = generate_dungeon(self.act, self.ascension, &mut self.rng.map);
                    self.pos = (0, 0);
                    self.encounter_idx = 0;
                    self.elite_idx = 0;
                    self.populate_act();
                }
            }
        }
    }
}

trait IfEmpty {
    fn if_empty(self, other: &'static [MonsterKind]) -> &'static [MonsterKind];
}
impl IfEmpty for &'static [MonsterKind] {
    fn if_empty(self, other: &'static [MonsterKind]) -> &'static [MonsterKind] {
        if self.is_empty() {
            other
        } else {
            self
        }
    }
}
