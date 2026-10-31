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
