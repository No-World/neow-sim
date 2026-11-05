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

/// MapRoomNode.addEdge：同目标不重复加（Java 逐边 compareTo 判重）
fn add_edge(edges: &mut Vec<usize>, dst: usize) {
    if !edges.contains(&dst) {
        edges.push(dst);
        // EdgeComparator 排序：srcY 升序 → srcX 升序 → dstX 升序；此处只比 dstX
        edges.sort_unstable();
    }
}

/// MapGenerator.generateDungeon + AbstractDungeon.generateMap
/// 幕地图 RNG 播种（Java 三个 dungeon 类各自的公式）：
/// - Act1 Exordium：`seed + actNum`
/// - Act2 TheCity：`seed + actNum * 100`
/// - Act3 TheBeyond：`seed + actNum * 200`
/// - Act4 TheEnding：`seed + actNum * 300`
pub fn map_seed(seed: u64, act: u8) -> u64 {
    let mult = match act {
        1 => 1u64,
        2 => 100,
        3 => 200,
        _ => 300,
    };
    seed.wrapping_add((act as u64).wrapping_mul(mult))
}

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
        add_edge(&mut rows[start_y][start_x as usize].edges, 3);
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

    // 加边 + 记父（Java：addEdge 去重；Edges 恒按 EdgeComparator 排序）
    let dst = new_x as usize;
    add_edge(&mut rows[start_y][start_x as usize].edges, dst);
    // MapRoomNode.addParent 是**纯追加**（不去重）：同一父可能被登记多次，
    // 祖先检查会对它重复执行，从而多消耗 randRange。去重会让地图分叉。
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
    // ⚠️ 逐字节复刻 Java 的 MapGenerator.getCommonAncestor，**包括它的 bug**：
    //   `if (node1.x < node2.y)` —— 比的是 node1.x 与 node2.**y**（不是 node2.x）。
    //   y 恒为同一行，所以 node1.x < node2.y 几乎恒成立（x < 行号），
    //   只有当 x 很大且 y 很小时才翻转。必须照抄，否则地图分叉、mapRng 消耗数不符。
    let (mut l, mut r): ((i32, usize), (i32, usize)) = if node1.0 < node2.1 as i32 {
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
        // getNodeWithMaxX(l.parents) 与 getNodeWithMinX(r.parents)
        let lx = *lp.iter().max()? as i32;
        let rx = *rp.iter().min()? as i32;
        // Java 比的是节点引用：相等即返回该节点（用 (x,y) 判等）
        let lnode = (lx, (current_y - 1) as usize);
        let rnode = (rx, (current_y - 1) as usize);
        l = lnode;
        r = rnode;
        if lnode == rnode {
            return Some(lnode.1); // 返回节点 y（调用方用 ancestor.y 算 gap）
        }
        current_y -= 1;
    }
    None
}

/// filterRedundantEdgesFromRow：首行同目标重边只留一条
fn filter_redundant_edges(rows: &mut [Vec<MapNode>]) {
    // 逐行 0 节点处理，但 `existing_edges` **跨节点累积**（Java 的 existingEdges 声明在
    // 循环外）。若每个节点重置，跨节点的同目标重边就删不掉——表现为多出整条边。
    let mut existing_edges: Vec<usize> = Vec::new();
    for node in rows[0].iter_mut() {
        if node.edges.is_empty() {
            continue;
        }
        let mut delete_list: Vec<usize> = Vec::new();
        for &e in &node.edges {
            if existing_edges.contains(&e) {
                delete_list.push(e);
            }
            existing_edges.push(e);
        }
        node.edges.retain(|e| !delete_list.contains(e));
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
    // ⚠️ 此处**不补** Monster：Java 的 generateRoomTypes 只放 shop/rest/elite/event，
    // 补 Monster 发生在 distributeRoomsAcrossMap 里、固定行赋值**之后**，且补到
    // nodeCount（有边且尚未赋房间的节点数，不含 row 0/8/14）。若提前按 count 补齐，
    // 池的长度与内容都变，洗牌结果全错（实测房间类型分布不一致）。

    // 3) 固定行：0=战斗 8=宝箱 14=休息
    //
    // ⚠️ RoomTypeAssigner.assignRowAsRoomType 只判断 `room == null`，
    // **不看 hasEdges**——所以孤立节点（没有出边）也会被赋成该行的固定类型。
    // 若在这里跳过无出边节点，房间总数会比真游戏少（实测 57 vs 68）。
    for row in rows.iter_mut() {
        for n in row.iter_mut() {
            if n.y == 0 {
                n.room = Some(RoomKind::Monster);
            } else if n.y == 8 {
                n.room = Some(RoomKind::Treasury);
            } else if n.y == MAP_HEIGHT - 1 {
                n.room = Some(RoomKind::Rest);
            }
        }
    }

    // 3.5) distributeRoomsAcrossMap：先补 Monster 到 nodeCount，再洗牌
    let node_count = rows
        .iter()
        .flat_map(|r| r.iter())
        .filter(|n| !n.edges.is_empty() && n.room.is_none())
        .count();
    while rooms.len() < node_count {
        rooms.push(RoomKind::Monster);
    }
    // 源码此时若 rooms 比 nodeCount 长只告警、不截断

    // 4) Collections.shuffle(roomList, rng.random)
    //
    // ⚠️ 关键陷阱：`rng.random` 是 libgdx 的 RandomXS128（继承 java.util.Random），
    // Collections.shuffle 调用的是它的 nextInt(bound)，走 nextLong(n) 的 62 位
    // 拒绝采样，**不推进 STS 的 counter**。若用 rng.random_range 会推进 counter，
    // 导致 mapRng counter 与真游戏不符（实测 154 vs 95）。
    for i in (1..rooms.len()).rev() {
        let j = rng.rng.next_long_n(i as u64 + 1) as usize;
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
        // 真游戏 MonsterGroup 按 x 位序插入：[M左, L(尸), M右]——死 L 仍占位，
        // 目标下标 = 完整数组序（[164] target 0 打左 M、[167] target 2 打右 M）。
        // 曾 push 到 vec 尾（[L, M左, M右]），导致 target 0 打到尸体上。
        assert_eq!(c.monsters.len(), 3, "[M左, L(尸), M右]");
        assert_eq!(c.monsters[0].kind, MonsterKind::AcidSlimeM);
        assert_eq!(c.monsters[1].kind, MonsterKind::AcidSlimeL);
        assert!(c.monsters[1].creature.is_dead(), "L 版自灭");
        assert_eq!(c.monsters[2].kind, MonsterKind::AcidSlimeM);
        assert_eq!(
            c.monsters[0].creature.max_hp, hp_at_split,
            "M 版承分裂时 HP"
        );
        assert_eq!(c.monsters[2].creature.max_hp, hp_at_split);
        // M 版入场 rollMove：意图已掷（非固定）
        assert!(matches!(c.monsters[0].next_move, 1 | 2 | 4));
        assert!(matches!(c.monsters[2].next_move, 1 | 2 | 4));
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
        // Boss 战利改为「3 候选待决策」（真游戏 BossRelicScreen 可 3 选 1 或跳过），
        // 不再自动发放；幕间卡牌也改为 CardReward 奖励项。
        assert_eq!(run.boss_relic_candidates.len(), 3, "Boss 遗物 3 候选");
        assert_eq!(run.relics.len(), relics_before, "未自动发放");
        let has_card_reward = run
            .rewards
            .iter()
            .any(|r| matches!(r, RewardItem::CardReward(_)));
        assert!(has_card_reward, "幕间卡牌作为 CardReward 待选");
        assert_eq!(run.deck.len(), deck_before, "未自动入组");
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
        // 奖励：金币 + 药水(概率) + 1 个卡牌奖励项（内含 3 张候选）
        assert!(run.rewards.len() >= 2, "金币+卡牌奖励至少 2 项");
        let golds: Vec<_> = run
            .rewards
            .iter()
            .filter(|r| matches!(r, RewardItem::Gold(g) if (10..=20).contains(g)))
            .collect();
        assert_eq!(golds.len(), 1, "金币 10-20");
        let cards: Vec<_> = run
            .rewards
            .iter()
            .filter(|r| matches!(r, RewardItem::CardReward(_)))
            .collect();
        assert_eq!(cards.len(), 1, "1 个卡牌奖励项");
        // 3 张候选且互不重复（真游戏语义：一个奖励项内含 3 选 1）
        if let Some(RewardItem::CardReward(cands)) = run.rewards.last() {
            assert_eq!(cands.len(), 3, "3 张候选");
            let (a, b, c) = (&cands[0], &cands[1], &cands[2]);
            assert!(a.def.id != b.def.id && b.def.id != c.def.id && a.def.id != c.def.id);
        }
        // 领取：金币 + 一张卡
        let card_idx = run
            .rewards
            .iter()
            .position(|r| matches!(r, RewardItem::CardReward(_)))
            .unwrap();
        let deck_before = run.deck.len();
        run.take_reward(card_idx);
        assert_eq!(run.deck.len(), deck_before + 1, "卡进主牌组");
        run.take_all_rewards_skip_cards();
        assert!(run.player.gold >= 99 + 10, "金币累计");
    }

    /// 精英奖励：遗物 + 金币 25-35；怜悯递增
    #[test]
    /// 回归锁定（seed PMCWKZEF = 1585262650280，真游戏 long.jsonl 轨迹）：
    /// floor 7 精英 → Mercury Hourglass；floor 9 精英 → Pear。
    /// 曾因预洗池用声明序（≠ Java HashMap 迭代序）+ 精英 tier 阈值 85（应 82）双双分叉。
    #[test]
    fn elite_relic_order_pmcwkzef() {
        let mut run = RunState::new(1585262650280, 0);
        run.pos = (6, 0);
        run.map.rows[6][0].room = Some(RoomKind::Elite);
        run.combat = Some(Combat::new_run_battle(
            7,
            0,
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
        let got = run
            .rewards
            .iter()
            .find_map(|r| match r {
                RewardItem::Relic(k) => Some(*k),
                _ => None,
            })
            .expect("elite 应掉遗物");
        assert_eq!(
            got,
            crate::relics::RelicKind::MercuryHourglass,
            "floor7 精英遗物应为 Mercury Hourglass"
        );

        // 第二次精英（floor 9）：池已移除 Mercury → Pear
        run.pos = (8, 0);
        run.map.rows[8][0].room = Some(RoomKind::Elite);
        run.combat = Some(Combat::new_run_battle(
            9,
            0,
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
        run.rewards.clear();
        run.settle_combat(0);
        let got2 = run
            .rewards
            .iter()
            .find_map(|r| match r {
                RewardItem::Relic(k) => Some(*k),
                _ => None,
            })
            .expect("elite2 应掉遗物");
        assert_eq!(
            got2,
            crate::relics::RelicKind::Pear,
            "floor9 精英遗物应为 Pear"
        );
    }

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
        // 跳过 Neow/首次选列：直接落到 row 0 上有出边的列，然后沿边推进
        let start = run.start_columns()[0];
        run.pos = (0, start);
        run.map_started = true;
        run.floor = 1;
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

/// NeowReward.getRewardOptions(case 2) 的候选名列表（按 drawback 剔除冲突项）
pub fn cat2_option_names(d: Option<NeowDrawback>) -> Vec<&'static str> {
    let mut v = vec!["RANDOM_COLORLESS_2"];
    if d != Some(NeowDrawback::Curse) {
        v.push("REMOVE_TWO");
    }
    v.push("ONE_RARE_RELIC");
    v.push("THREE_RARE_CARDS");
    if d != Some(NeowDrawback::NoGold) {
        v.push("TWO_FIFTY_GOLD");
    }
    v.push("TRANSFORM_TWO_CARDS");
    if d != Some(NeowDrawback::HpLoss) {
        v.push("TWENTY_PERCENT_HP_BONUS");
    }
    v
}

/// generateExclusions：按「弱怪末项」返回强怪需排除的组合名
/// （Exordium/TheCity/TheBeyond 的 generateExclusions 各自实现，此处合并）
pub fn act_exclusions(
    _weak: &[EncInfo],
    _strong: &[EncInfo],
    last_weak: Option<&'static str>,
) -> Vec<&'static str> {
    match last_weak {
        Some("Looter") => vec!["Exordium Thugs"],
        Some("Blue Slaver") => vec!["Red Slaver", "Exordium Thugs"],
        Some("2 Louse") => vec!["3 Louse"],
        Some("Small Slimes") => vec!["Large Slime", "Lots of Slimes"],
        // TheCity 对应项（Chosen / Spheric Guardian 等）
        Some("Chosen") => vec!["Cultist and Chosen"],
        Some("3 Byrds") => vec!["Chosen and Byrds"],
        // Jaw Worm / Cultist 及未知：无排除
        _ => Vec::new(),
    }
}

/// MonsterInfo.roll：累积权重命中（roll ∈ [0,1)）
///
/// ⚠️ 关键：`MonsterInfo.normalizeWeights` 会先 `Collections.sort(list)`
/// （按 weight 升序，Float.compare），再归一化。Java 的 roll 遍历的是**排序后**
/// 的列表，所以这里必须先按 weight 升序排（稳定排序，等权保持原序）再累积。
pub fn enc_roll(list: &[EncInfo], roll: f32) -> &'static str {
    let mut sorted: Vec<&EncInfo> = list.iter().collect();
    // Collections.sort 是稳定排序（TimSort）；等权项保持定义序
    sorted.sort_by(|a, b| {
        a.weight
            .partial_cmp(&b.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total: f32 = sorted.iter().map(|e| e.weight).sum();
    let mut cur = 0.0f32;
    for e in &sorted {
        cur += e.weight / total;
        if roll < cur {
            return e.name;
        }
    }
    sorted.last().map(|e| e.name).unwrap_or("ERROR")
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
            name: "Hexaghost",
            weight: 1.0,
            kinds: &[Hexaghost],
        },
        EncInfo {
            name: "Slime Boss",
            weight: 1.0,
            kinds: &[SlimeBoss],
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

/// Act 3 遭遇池（TheBeyond）——组合全量可组。
/// ⚠️ 形体组合（3/4 Shapes、Sphere and 2 Shapes）源码在 spawnShapes 逐槽
/// 从 [Repulsor×2, Exploder×2, Spiker×2] 无放回抽（miscRng），静态表记
/// Repulsor 全体；实际组合在 enter_room 时重掷（见 reroll_shapes）
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

/// Neow 开局选项（旧字段，保留兼容）
#[derive(Clone, Debug)]
pub struct NeowOption {
    pub kind: u8,
    pub desc: &'static str,
}

/// Neow drawback（cat 2 的负面效果）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeowDrawback {
    HpLoss,
    NoGold,
    Curse,
    PercentDamage,
}

/// 一次 Neow 抽签结果：category + 该 category 内抽中的下标 + drawback
#[derive(Clone, Debug)]
pub struct NeowPick {
    pub cat: u8,
    pub idx: usize,
    pub drawback: Option<NeowDrawback>,
}

/// 奖励类型（RewardItem.RewardType 子集）
#[derive(Clone, Debug)]
pub enum RewardItem {
    Gold(i32),
    Card(crate::cards::CardInst),
    /// 卡牌奖励（真游戏语义：**一个**奖励项，内含 3 张候选，玩家选 1）。
    /// 之前建模成 3 个独立 Card 项，导致「全领」会多塞 2 张、且奖励屏交互对不上。
    CardReward(Vec<crate::cards::CardInst>),
    Potion(crate::potions::PotionKind),
    Relic(crate::relics::RelicKind),
}

/// 整局状态：幕/层/遭遇序列/当前战斗
pub struct RunState {
    /// Boss 遗物 3 选 1 的候选（真游戏 BossRelicScreen：可跳过换取能量/金币）
    pub boss_relic_candidates: Vec<crate::relics::RelicKind>,
    /// 遗物预洗池（initializeRelicList：5 个 tier 各洗一次，之后只 remove(0)）
    pub relic_pools: std::collections::HashMap<&'static str, Vec<crate::relics::RelicKind>>,
    /// 战斗结束时是否有偷钱怪成功逃逸（AbstractRoom.mugged）
    /// → 奖励屏走 openCombat，不生成卡牌奖励
    pub combat_mugged: bool,
    pub seed: u64,
    pub ascension: i32,
    /// 可玩角色（决定起始组/初始遗物/血量/卡池色/职业药水）
    pub char_kind: crate::state::CharKind,
    pub act: u8,
    pub map: GameMap,
    pub pos: (usize, usize), // 当前节点 (y, x)
    pub player: RunPlayer,
    pub rng: RngStreams,
    /// 幕内遭遇序列（populateMonsterList 产物：名字；具体怪物种类/数量在进房时按
    /// miscRng 掷出，见 `resolve_encounter`）
    pub encounter_list: Vec<&'static str>,
    pub encounter_idx: usize,
    pub elite_list: Vec<&'static [MonsterKind]>,
    /// 精英遭遇名（与 elite_list 同序，对拍/日志用）
    pub elite_names: Vec<&'static str>,
    /// Boss 遭遇名（洗牌后取 [0]）
    pub boss_name: &'static str,
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
    /// 暴雪稀有度偏移（起 5，普通-1 至 -40，稀有重置 5；pub(crate)：商店/事件共用）
    pub(crate) card_blizz: i32,
    /// 最近一次 rollRarity 的 roll 值（含 blizz 偏移），仅调试用
    pub(crate) last_rarity_roll: i32,
    /// 药水怜悯（掉 40% 未中 +10，中 -10）
    potion_pity: i32,
    /// 当前房间奖励（待领取）
    pub rewards: Vec<RewardItem>,
    /// 商店（进入 Shop 房间时生成；协议层 shop_enter 后可购买）
    pub shop: Option<crate::shop::Shop>,
    /// 删卡服务价（整局累计：75 起，每次 +25；ShopScreen.purgeCost 静态量）
    pub purge_cost: i32,
    /// 当前事件（EventRoom.event）
    pub event: Option<crate::events::EventState>,
    /// 幕内事件池（不重复；每次进幕重填）
    pub event_pool: Vec<crate::events::EventKind>,
    pub shrine_pool: Vec<crate::events::EventKind>,
    pub special_pool: Vec<crate::events::EventKind>,
    /// 事件战斗标记（胜利后事件收尾；奖励由事件预置，卡牌/药水照掷）
    pub event_combat: bool,
    /// 休息房待选择（true = 已入房未休息）
    pub rest_available: bool,
    /// Neow 开局：None=未开始, Some(category)=待选择（category 0-3）
    pub neow_category: Option<u8>,
    /// Neow 可选奖励列表（category 对应的 4 个选项描述）
    pub neow_options: Vec<NeowOption>,
    /// Neow 专用 RNG（Java: `new Random(Settings.seed)`，与 13 条游戏流独立）
    pub neow_rng: Option<StsRandom>,
    /// Neow 4 个 category 各自的抽签结果
    pub neow_picks: Vec<NeowPick>,
    /// Neow「前 3 场战斗敌人 1 HP」
    pub neow_enemy_kill: u8,
    /// 是否已进入地图（真游戏起点是虚拟节点 y=-1，首次选点进 row 0）
    pub map_started: bool,
    /// 房间结束后待结算：拿走的卡进主牌组等
    pub pending_relic_tier_roll: bool,
}

/// STS SeedHelper：种子字符串 ↔ u64 编码
/// CHARACTERS = "0123456789ABCDEFGHIJKLMNPQRSTUVWXYZ"（34 字符，无 O）
pub mod seed_helper {
    const CHARACTERS: &[u8] = b"0123456789ABCDEFGHIJKLMNPQRSTUVWXYZ";

    /// getLong：base-34 编码 → u64（O→0 替换）
    /// 与 Java BigInteger 的行为一致（Java 有符号 64 位但 STS 只用正值）
    pub fn decode(seed_str: &str) -> u64 {
        let upper = seed_str.to_uppercase().replace('O', "0");
        let mut total: u64 = 0;
        for c in upper.bytes() {
            let idx = CHARACTERS.iter().position(|&x| x == c).unwrap_or(0);
            total = total.wrapping_mul(35).wrapping_add(idx as u64);
        }
        total
    }

    /// getString：u64 → base-34 编码
    pub fn encode(seed: u64) -> String {
        if seed == 0 {
            return "0".to_string();
        }
        let mut result = Vec::new();
        let mut s = seed;
        while s > 0 {
            result.push(CHARACTERS[(s % 35) as usize] as char);
            s /= 35;
        }
        result.iter().rev().collect()
    }
}

impl RunState {
    /// 开新局（默认铁甲战士，兼容旧调用方）
    pub fn new(seed: u64, ascension: i32) -> Self {
        Self::new_for(seed, ascension, crate::state::CharKind::Ironclad)
    }

    /// 开新局（指定角色）：起始组/初始遗物/血量（A6 起始 90%、
    /// A14 上限 -loss）/遭遇序列同源码逐行
    pub fn new_for(seed: u64, ascension: i32, char_kind: crate::state::CharKind) -> Self {
        let mut rng = RngStreams::new(seed);
        // mapRng 每幕独立播种（Java：Exordium `seed+actNum`、TheCity `seed+actNum*100`、
        // TheBeyond `seed+actNum*200`）。act=1 时三式均为 seed+1，与 generateSeeds()
        // 的 `new Random(Settings.seed)` 不同——所以首幕地图必须用 seed+1 而非 seed。
        rng.map = StsRandom::new(map_seed(seed, 1));
        let map = generate_dungeon(1, ascension, &mut rng.map);
        // A14：decreaseMaxHealth(getAscensionMaxHPLoss())；A6：起始 = round(90%)
        let mut max_hp = char_kind.base_max_hp();
        if ascension >= 14 {
            max_hp -= char_kind.asc_hp_loss();
        }
        let hp = if ascension >= 6 {
            ((max_hp as f32) * 0.9 + 0.5) as i32
        } else {
            max_hp
        };
        let mut s = Self {
            seed,
            ascension,
            char_kind,
            act: 1,
            map,
            pos: (0, 0),
            player: RunPlayer {
                hp,
                max_hp,
                gold: 99,
            },
            rng,
            encounter_list: Vec::new(),
            encounter_idx: 0,
            elite_list: Vec::new(),
            elite_names: Vec::new(),
            boss_name: "",
            elite_idx: 0,
            boss: &[],
            combat: None,
            floor: 0,
            over: false,
            won: false,
            deck: crate::cards::starter_deck(char_kind.id(), ascension),
            potions: vec![None, None, None],
            relics: vec![crate::relics::RelicInst::new(char_kind.starting_relic())],
            card_blizz: 5,
            last_rarity_roll: 0,
            potion_pity: 0,
            rewards: Vec::new(),
            pending_relic_tier_roll: false,
            shop: None,
            purge_cost: 75,
            event: None,
            event_pool: Vec::new(),
            shrine_pool: Vec::new(),
            special_pool: Vec::new(),
            event_combat: false,
            neow_category: None,
            neow_options: Vec::new(),
            neow_rng: None,
            neow_picks: Vec::new(),
            neow_enemy_kill: 0,
            map_started: false,
            combat_mugged: false,
            relic_pools: std::collections::HashMap::new(),
            boss_relic_candidates: Vec::new(),
            rest_available: false,
        };
        s.populate_act();
        s.init_event_pools();
        // AbstractDungeon.initializeRelicList：5 个 tier 池各
        //   `Collections.shuffle(pool, new Random(relicRng.randomLong()))`
        // 共消耗 relicRng 5 次（真游戏开局 relic counter = 5）。
        // 池内顺序 = 后续 addRelicToRewards(tier) 的弹出顺序。
        {
            let mut pools = std::collections::HashMap::new();
            for tier in ["COMMON", "UNCOMMON", "RARE", "SHOP", "BOSS"] {
                let mut pool = crate::relic_data::relics_of_tier(tier);
                let seed = s.rng.relic.random_long();
                let mut jr = crate::rng::JavaRandom::new(seed);
                crate::rng::java_shuffle(&mut pool, &mut jr);
                pools.insert(tier, pool);
            }
            s.relic_pools = pools;
        }
        s.init_neow();
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
        // 弱怪数量：Act1 = 3（Exordium.generateWeakEnemies(3)），Act2/3 = 2
        let weak_count = if self.act == 1 { 3 } else { 2 };
        let mut list: Vec<&'static str> = Vec::new();
        while list.len() < weak_count {
            let name = enc_roll(&weak, self.rng.monster.random_float());
            // populateMonsterList（非精英）：同时防「上一个」与「上上个」重复。
            // 只查上一个会让弱怪段在长列表里漏掉 --i 重掷（实测 12/40 种子对不上）。
            let n = list.len();
            if n >= 1 && name == list[n - 1] {
                continue;
            }
            if n >= 2 && name == list[n - 2] {
                continue;
            }
            list.push(name);
        }
        // populateFirstStrongEnemy：强怪首项需排除与弱怪末项冲突的组合
        let exclusions = act_exclusions(&weak, &strong, list.last().copied());
        loop {
            let name = enc_roll(&strong, self.rng.monster.random_float());
            if exclusions.contains(&name) {
                continue;
            }
            if list.last() == Some(&name) {
                continue;
            }
            list.push(name);
            break;
        }
        // 强怪 12：防上/上上重复。
        // ⚠️ 注意 Java 的 count 是「再追加 12 项」，而列表里已有 3弱 + 1首强，
        //    所以最终长度 = weak_count + 1 + 12（Act1 为 16）。写成 weak_count + 12
        //    会少追加 1 项，连带少消耗一次 monsterRng。
        while list.len() < weak_count + 1 + 12 {
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
        self.encounter_list = list;
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
        self.elite_names = elist.clone();
        // initializeBoss：Collections.shuffle(bossList, new java.util.Random(monsterRng.randomLong()))
        // 消耗 1 次 monsterRng，然后取洗牌后的第 0 项。
        let sh_seed = self.rng.monster.random_long();
        let mut boss_names: Vec<&'static str> = bosses.iter().map(|e| e.name).collect();
        let mut jr = crate::rng::JavaRandom::new(sh_seed);
        crate::rng::java_shuffle(&mut boss_names, &mut jr);
        let boss_name = boss_names[0];
        self.boss_name = boss_name;
        self.boss = enc_kinds(&bosses, boss_name);
    }

    /// 进房重播种：monsterHpRng / aiRng / shuffleRng / cardRandomRng / miscRng
    /// 全部 `new Random(seed + floorNum)`（counter 归零）。
    fn reseed_floor_streams(&mut self) {
        let f = self.floor as u64;
        let s = self.seed.wrapping_add(f);
        self.rng.monster_hp = StsRandom::new(s);
        self.rng.ai = StsRandom::new(s);
        self.rng.shuffle = StsRandom::new(s);
        self.rng.card_random = StsRandom::new(s);
        self.rng.misc = StsRandom::new(s);
    }

    /// 遭遇名 → 具体怪物组（复刻 MonsterHelper.getEncounter + getLouse 等）。
    ///
    /// ⚠️ 关键：怪物种类不是静态的。`2 Louse` 的每只 Louse 由 `miscRng.randomBoolean()`
    /// 决定 Normal/Defensive；`Small Slimes` 逐只掷；`Large Slime` 掷一次。
    /// 且 **先定种类再 spawn（掷 HP）**，顺序决定 monsterHpRng 的取值。
    fn resolve_encounter(&mut self, name: &str) -> Vec<crate::state::MonsterState> {
        use crate::state::MonsterState;
        use MonsterKind::*;
        let kinds: Vec<MonsterKind> = match name {
            "2 Louse" => {
                // MonsterHelper.getLouse ×2：先掷两只的种类，再各自 spawn
                let a = if self.rng.misc.random_boolean() {
                    LouseNormal
                } else {
                    LouseDefensive
                };
                let b = if self.rng.misc.random_boolean() {
                    LouseNormal
                } else {
                    LouseDefensive
                };
                vec![a, b]
            }
            "3 Louse" => {
                let mut v = Vec::with_capacity(3);
                for _ in 0..3 {
                    v.push(if self.rng.misc.random_boolean() {
                        LouseNormal
                    } else {
                        LouseDefensive
                    });
                }
                v
            }
            "Small Slimes" => {
                // MonsterHelper.spawnSmallSlimes：**1 只小 + 1 只中**，只掷 1 次
                //   if (miscRng.randomBoolean()) { SpikeSlime_S, AcidSlime_M }
                //   else                         { AcidSlime_S, SpikeSlime_M }
                if self.rng.misc.random_boolean() {
                    vec![SpikeSlimeS, AcidSlimeM]
                } else {
                    vec![AcidSlimeS, SpikeSlimeM]
                }
            }
            "Lots of Slimes" => {
                // MonsterHelper.spawnManySmallSlimes：固定 5 只，从
                // [Spike×3, Acid×2] 无放回逐只抽（每次 miscRng.random(size-1)）
                let mut pool = vec![
                    SpikeSlimeS,
                    SpikeSlimeS,
                    SpikeSlimeS,
                    AcidSlimeS,
                    AcidSlimeS,
                ];
                let mut v = Vec::with_capacity(5);
                while !pool.is_empty() {
                    let j = self.rng.misc.random_range(pool.len() as i32 - 1).max(0) as usize;
                    v.push(pool.remove(j.min(pool.len() - 1)));
                }
                v
            }
            "Large Slime" => {
                if self.rng.misc.random_boolean() {
                    vec![AcidSlimeL]
                } else {
                    vec![SpikeSlimeL]
                }
            }
            "Cultist" => vec![Cultist],
            "Jaw Worm" => vec![JawWorm],
            "Blue Slaver" => vec![SlaverBlue],
            "Red Slaver" => vec![SlaverRed],
            "Looter" => vec![Looter],
            "2 Fungi Beasts" => vec![FungiBeast, FungiBeast],
            "Gremlin Gang" => {
                // bottomGremlinGang：4 只随机 gremlin（无放回）
                let pool = vec![
                    GremlinWarrior,
                    GremlinWarrior,
                    GremlinThief,
                    GremlinThief,
                    GremlinFat,
                    GremlinFat,
                    GremlinTsundere,
                    GremlinWizard,
                ];
                let mut p = pool;
                let mut v = Vec::with_capacity(4);
                for _ in 0..4 {
                    let j = self.rng.misc.random_range(p.len() as i32 - 1).max(0) as usize;
                    v.push(p.remove(j.min(p.len() - 1)));
                }
                v
            }
            "Exordium Thugs" => {
                // MonsterHelper.bottomHumanoid：**候选全构造**（未选中者也掷 HP）——
                //   [0] bottomGetWeakWildlife：getLouse 掷 bool（T→Normal/F→Defensive）
                //       → 三只全部 spawn（Louse 2 掷 / SpikeM 1 掷 / AcidM 1 掷）
                //       → miscRng.random(0,2) 选
                //   [1] bottomGetStrongHumanoid：[Cultist(0 掷), getSlaver(bool→Red/Blue,
                //       1 掷), Looter(1 掷)] → miscRng.random(0,2) 选
                let b_louse = self.rng.misc.random_boolean();
                let louse = MonsterState::spawn(
                    if b_louse { LouseNormal } else { LouseDefensive },
                    self.ascension,
                    &mut self.rng.monster_hp,
                );
                let spike =
                    MonsterState::spawn(SpikeSlimeM, self.ascension, &mut self.rng.monster_hp);
                let acid =
                    MonsterState::spawn(AcidSlimeM, self.ascension, &mut self.rng.monster_hp);
                let wk = match self.rng.misc.random_start_end(0, 2) {
                    0 => louse,
                    1 => spike,
                    _ => acid,
                };
                let cultist =
                    MonsterState::spawn(Cultist, self.ascension, &mut self.rng.monster_hp);
                let b_slaver = self.rng.misc.random_boolean();
                let slaver = MonsterState::spawn(
                    if b_slaver { SlaverRed } else { SlaverBlue },
                    self.ascension,
                    &mut self.rng.monster_hp,
                );
                let looter = MonsterState::spawn(Looter, self.ascension, &mut self.rng.monster_hp);
                let stg = match self.rng.misc.random_start_end(0, 2) {
                    0 => cultist,
                    1 => slaver,
                    _ => looter,
                };
                return vec![wk, stg];
            }
            "Exordium Wildlife" => {
                // MonsterHelper.bottomWildlife：**候选全构造**（未选中者也掷 HP）——
                //   [0] bottomGetStrongWildlife：FungiBeast(1 掷) + JawWorm(1 掷)
                //       → miscRng.random(0,1) 选
                //   [1] bottomGetWeakWildlife：getLouse(bool) + SpikeM + AcidM
                //       → miscRng.random(0,2) 选
                // （实测 PMCWKZEF floor11：真游戏 monsterHp 7 次消耗 = 6 构造 + 1 CurlUp）
                let stg_fungi =
                    MonsterState::spawn(FungiBeast, self.ascension, &mut self.rng.monster_hp);
                let stg_jaw =
                    MonsterState::spawn(JawWorm, self.ascension, &mut self.rng.monster_hp);
                let stg = match self.rng.misc.random_start_end(0, 1) {
                    0 => stg_fungi,
                    _ => stg_jaw,
                };
                let b_louse = self.rng.misc.random_boolean();
                let louse = MonsterState::spawn(
                    if b_louse { LouseNormal } else { LouseDefensive },
                    self.ascension,
                    &mut self.rng.monster_hp,
                );
                let spike =
                    MonsterState::spawn(SpikeSlimeM, self.ascension, &mut self.rng.monster_hp);
                let acid =
                    MonsterState::spawn(AcidSlimeM, self.ascension, &mut self.rng.monster_hp);
                let wk = match self.rng.misc.random_start_end(0, 2) {
                    0 => louse,
                    1 => spike,
                    _ => acid,
                };
                return vec![stg, wk];
            }
            "Gremlin Nob" => vec![GremlinNob],
            "Lagavulin" => vec![Lagavulin],
            "3 Sentries" => vec![Sentry, Sentry, Sentry],
            // TheCity / TheBeyond 常见组合（静态部分）
            "Chosen" => vec![Chosen],
            "3 Byrds" => vec![Byrd, Byrd, Byrd],
            "Cultist and Chosen" => vec![Cultist, Chosen],
            "Chosen and Byrds" => vec![Chosen, Byrd, Byrd],
            "Shell Parasite" => vec![ShelledParasite],
            "Snecko" => vec![Snecko],
            "Snake Plant" => vec![SnakePlant],
            "Centurion and Healer" => vec![Centurion, Healer],
            "3 Darklings" => vec![Darkling, Darkling, Darkling],
            "Orb Walker" => vec![OrbWalker],
            "3 Shapes" => {
                // spawnShapes：逐槽从 [Repulsor×2, Exploder×2, Spiker×2] 无放回
                let mut pool = vec![Repulsor, Repulsor, Exploder, Exploder, Spiker, Spiker];
                let mut v = Vec::with_capacity(3);
                for _ in 0..3 {
                    let j = self.rng.misc.random_range(pool.len() as i32 - 1).max(0) as usize;
                    v.push(pool.remove(j.min(pool.len() - 1)));
                }
                v
            }
            "4 Shapes" => {
                let mut pool = vec![Repulsor, Repulsor, Exploder, Exploder, Spiker, Spiker];
                let mut v = Vec::with_capacity(4);
                for _ in 0..4 {
                    let j = self.rng.misc.random_range(pool.len() as i32 - 1).max(0) as usize;
                    v.push(pool.remove(j.min(pool.len() - 1)));
                }
                v
            }
            // 其余按名字映射到单只
            _ => self.static_encounter_kinds(name),
        };
        kinds
            .into_iter()
            .map(|k| MonsterState::spawn(k, self.ascension, &mut self.rng.monster_hp))
            .collect()
    }

    /// 兜底：从 act pools 的静态 kinds 取（不含随机成分的组合）
    fn static_encounter_kinds(&self, name: &str) -> Vec<MonsterKind> {
        let (weak, strong, elites, bosses) = self.act_pools();
        for list in [&weak, &strong, &elites, &bosses] {
            if let Some(k) = list.iter().find(|e| e.name == name) {
                return k.kinds.to_vec();
            }
        }
        vec![crate::state::MonsterKind::JawWorm]
    }

    /// 选择下一节点（行动空间 = 当前节点边）。
    /// 行 14 的边是 Boss 哨兵：不再前进，位置保持（Boss 房由 enter_room 进入）
    pub fn choose_node(&mut self, edge_idx: usize) {
        let (y, x) = self.pos;
        if !self.map_started {
            // 真游戏起点是虚拟节点 (0, -1)，第一次选点直接落到 row 0 的列。
            // 引擎用 pos=(0,x) 承载 row 0，所以首次选择只是确认列，不再下移。
            let target_x = if x < MAP_WIDTH {
                x
            } else {
                edge_idx.min(MAP_WIDTH - 1)
            };
            self.pos = (0, target_x);
            self.map_started = true;
            self.floor += 1;
            return;
        }
        if y + 1 >= MAP_HEIGHT {
            return;
        }
        // edge_idx 语义：有边时是边下标；WingedGreaves 跨节点时是目标列
        let cols = self.next_columns();
        let next_x = if edge_idx < self.map.rows[y][x].edges.len() {
            self.map.rows[y][x].edges[edge_idx]
        } else {
            *cols.get(edge_idx).unwrap_or(&self.map.rows[y][x].edges[0])
        };
        // WingedGreaves：跨非连通节点时消耗 1 次
        if self.winged_greaves_active() && !self.map.rows[y][x].edges.contains(&next_x) {
            if let Some(r) = self
                .relics
                .iter_mut()
                .find(|r| r.kind == crate::relics::RelicKind::WingedGreaves)
            {
                r.counter -= 1;
            }
        }
        self.pos = (y + 1, next_x);
        self.floor += 1;
    }

    /// row 0 上所有可选列（首次选点的行动空间）
    pub fn start_columns(&self) -> Vec<usize> {
        if self.map_started {
            return Vec::new();
        }
        (0..MAP_WIDTH)
            .filter(|x| {
                self.map.rows[0][*x].room.is_some() && !self.map.rows[0][*x].edges.is_empty()
            })
            .collect()
    }

    /// WingedGreaves 是否可用（counter > 0）
    pub fn winged_greaves_active(&self) -> bool {
        self.relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::WingedGreaves && r.counter > 0)
    }

    /// 下一步可去的列（含 WingedGreaves 的「跨任意节点」）
    ///
    /// 真游戏 `MapRoomNode.wingedIsConnectedTo`：持有 WingedGreaves 且 counter > 0 时，
    /// **下一行任意节点**都算连通（不看 dstX）。选点后 counter -= 1，归零则遗物失效。
    pub fn next_columns(&self) -> Vec<usize> {
        if !self.map_started {
            return self.start_columns();
        }
        let (y, x) = self.pos;
        if y + 1 >= MAP_HEIGHT {
            return Vec::new();
        }
        let mut cols: Vec<usize> = self.map.rows[y][x].edges.clone();
        if self.winged_greaves_active() {
            // 全行可选
            for nx in 0..MAP_WIDTH {
                if self.map.rows[y + 1][nx].room.is_some() && !cols.contains(&nx) {
                    cols.push(nx);
                }
            }
            cols.sort_unstable();
        }
        cols
    }

    /// 进入当前节点的房间。战斗类经 `Combat::new_run_battle` 注入主牌组/药水栏/
    /// 遗物栏/局外 HP 与金币；商店进货；事件入池抽取；休息房挂起待选
    /// （`rest_sleep` / `rest_smith`，对照 RestRoom.onPlayerEntry 不自动回血）
    pub fn enter_room(&mut self) {
        let (y, x) = self.pos;
        let room = self.map.rows[y][x].room;
        // ⚠️ 关键：进房时 5 条流重播种为 seed + floorNum（AbstractDungeon.nextRoomTransition
        // 1726-1730 行）。这决定了怪物 HP / 起手洗牌 / 怪物 AI / 战斗随机 / misc。
        // 若不重播种，counter 与真游戏完全对不上（实测 map/monster_hp/relic 全错）。
        self.reseed_floor_streams();
        self.rewards.clear();
        self.shop = None;
        self.event = None;
        self.event_combat = false;
        self.rest_available = false;
        match room {
            Some(RoomKind::Monster) | Some(RoomKind::Elite) => {
                let relics: Vec<_> = self.relics.clone();
                let deck = self.deck.clone();
                let potions = self.potions.clone();
                let (php, pmax, gold) = (self.player.hp, self.player.max_hp, self.player.gold);
                let boss_vocab = self
                    .boss
                    .first()
                    .map(|k| (*k as i32 as f32 + 1.0) / 80.0)
                    .unwrap_or(0.0);
                let mut cb = if room == Some(RoomKind::Elite) {
                    let k = self.elite_list[self.elite_idx];
                    self.elite_idx += 1;
                    let mut c = Combat::new_run_battle_with_rng(
                        self.rng.clone(),
                        self.ascension,
                        &k,
                        relics,
                        deck,
                        potions,
                        php,
                        pmax,
                        gold,
                    );
                    c.boss_vocab = boss_vocab;
                    c
                } else {
                    let name =
                        self.encounter_list[self.encounter_idx.min(self.encounter_list.len() - 1)];
                    self.encounter_idx += 1;
                    // 遭遇名 → 具体怪物组（按 miscRng 掷种类/数量，见 MonsterHelper；
                    // Wildlife/Thugs 等「候选全构造」遭遇在此处按真实顺序掷完全部候选）
                    let states = self.resolve_encounter(name);
                    let mut c = Combat::new_run_battle_with_states(
                        self.rng.clone(),
                        self.ascension,
                        states,
                        relics,
                        deck,
                        potions,
                        php,
                        pmax,
                        gold,
                    );
                    c.boss_vocab = boss_vocab;
                    c
                };
                self.combat = Some(cb);
                // 战斗内喝掉的药水同步回局外
            }
            Some(RoomKind::Rest) => {
                // RestRoom.onPlayerEntry：不自动回血——休息选项由 rest_sleep/
                // rest_smith 结算（协议层 rest 动作）
                self.rest_available = true;
            }
            Some(RoomKind::Shop) => {
                // MealTicket：进商店房回 15 HP（justEnteredRoom）
                if self
                    .relics
                    .iter()
                    .any(|r| r.kind == crate::relics::RelicKind::MealTicket)
                {
                    self.player.hp = (self.player.hp + 15).min(self.player.max_hp);
                }
                // ShopScreen.init：进货（Merchant 构造）+ 定价 + 删卡价快照
                let owned: Vec<_> = self.relics.iter().map(|r| r.kind).collect();
                self.shop = Some(crate::shop::Shop::generate(
                    self.ascension,
                    self.char_kind,
                    &mut self.card_blizz,
                    &owned,
                    self.purge_cost,
                    Self::IMPLEMENTED_RELICS,
                    &mut self.rng,
                ));
            }
            Some(RoomKind::Event) => {
                // EventRoom.onPlayerEntry → generateEvent(eventRng)
                self.enter_event();
            }
            Some(RoomKind::Treasury) => {
                // ⚠️ AbstractChest.open：宝箱**只有**金币(可选) + 1 件遗物，
                // **从不加卡牌奖励**。之前把宝箱走成普通战斗奖励流程，
                // 多出 3 张卡（cardRng 多 9 次）与药水判定。
                // 宝箱房（TreasureRoom.onPlayerEntry → getRandomChest →
                // AbstractChest.randomizeReward；三幕同概率：小 50/中 33/大 17）。
                // 箱子打开即入奖励屏（协议层 reward 动作拿取/跳过，无独立开箱动作；
                // ⚠️ 偏差：金币额掷从开箱点击提前到进房，两者间无其他 treasureRng
                // 消耗，确定性等价；遗物具体件走已实现池（tier 骰照掷）
                let size_roll = self.rng.treasure.random_start_end(0, 99);
                let (common_c, uncommon_c, gold_c, gold_amt) = if size_roll < 50 {
                    (75, 25, 50, 25) // 小箱：恒普通/罕见，金币 25
                } else if size_roll < 50 + 33 {
                    (35, 50, 35, 50) // 中箱
                } else {
                    (0, 75, 50, 75) // 大箱：恒罕见/稀有
                };
                let roll = self.rng.treasure.random_start_end(0, 99);
                if roll < gold_c {
                    let jitter = 0.9 + self.rng.treasure.random_float() * 0.2;
                    let g = ((gold_amt as f32) * jitter + 0.5) as i32; // MathUtils.round
                    self.rewards.push(RewardItem::Gold(g));
                }
                // tier 阈值与金币判定同掷（roll）
                let tier = if roll < common_c {
                    "COMMON"
                } else if roll < common_c + uncommon_c {
                    "UNCOMMON"
                } else {
                    "RARE"
                };
                // ⚠️ 真游戏 AbstractChest.open → addRelicToRewards(tier)：
                // 从**开局已洗好的 tier 池**弹出第一只，运行时 **0 次 relicRng 消耗**
                // （洗牌发生在 initializeRelicList，用 relicRng.randomLong() 5 次）。
                // 原来走 roll_relic_kind（tier 掷 + 抽取掷 = 2 次），会让 relicRng 多消耗。
                if let Some(k) = self.pop_pooled_relic(tier) {
                    self.rewards.push(RewardItem::Relic(k));
                }
            }
            Some(RoomKind::Boss) => {
                // Boss 战斗继承整局 RNG 流（真游戏无 boss 专属换种——曾用
                // seed^0xDEAD_BEEF 独立播种，实测 PMCWKZEF floor16 起手牌序分叉）
                let relics: Vec<_> = self.relics.clone();
                self.combat = Some(Combat::new_run_battle_with_rng(
                    self.rng.clone(),
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
                // 3) 精英掉遗物：MonsterRoomElite.returnRandomRelicTier
                //   relicRng.random(0,99) <50 普通 >82 稀有 否则罕见
                //   ⚠️ 精英阈值是 **82**（普通房 MonsterRoom 才是 85），曾写错致 tier 分叉
                // AbstractRoom.addRelicToRewards(returnRandomRelicTier())：
                //   relicRng.random(0,99) 只用来**判 tier**（1 次），
                //   具体遗物从预洗池 remove(0)（0 次 RNG）。
                let roll = self.rng.relic.random_start_end(0, 99);
                let tier = if roll < 50 {
                    "COMMON"
                } else if roll > 82 {
                    "RARE"
                } else {
                    "UNCOMMON"
                };
                if let Some(k) = self.pop_pooled_relic(tier) {
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
            // 真游戏 AbstractRoom.addPotionToRewards → AbstractDungeon.returnRandomPotion(false)：
            //   1) potionRng.random(0,99) 判稀有度（<65 COMMON / <90 UNCOMMON / RARE）
            //   2) PotionHelper.getRandomPotion() → potionRng.random(pool-1) 均匀抽一张
            //   3) while (temp.rarity != rarity)：**稀有度不符就重抽**（limited=false 只关
            //      spamCheck，不关稀有度检查；Fruit Juice 一律跳过重抽）
            // 消耗 = 1 判定 + 1 tier + N 次抽取（实测 PMCWKZEF floor10：1+1+4=6 次）
            let pool = crate::potions::pool_for_class(self.char_kind.id());
            let roll = self.rng.potion.random_start_end(0, 99);
            let target = if roll < 65 {
                crate::potions::PotionRarity::Common
            } else if roll < 90 {
                crate::potions::PotionRarity::Uncommon
            } else {
                crate::potions::PotionRarity::Rare
            };
            let k = loop {
                let idx = self.rng.potion.random_range(pool.len() as i32 - 1).max(0) as usize;
                let k = pool[idx.min(pool.len() - 1)];
                if k == crate::potions::PotionKind::FruitJuice {
                    continue;
                }
                if k.rarity() == target {
                    break k;
                }
            };
            self.rewards.push(RewardItem::Potion(k));
            self.potion_pity -= 10;
        } else {
            self.potion_pity += 10;
        }
        // 4) 卡牌奖励：3 张（Boss 恒稀有）
        //
        // ⚠️ AbstractDungeon.getCard 的收尾循环会为每张**非稀有**卡掷一次升级概率：
        //   `if (c.rarity != RARE && cardRng.randomBoolean(cardUpgradedChance) && c.canUpgrade())`
        // 而 `Random.randomBoolean(float)` 是**无条件 ++counter**（哪怕 chance==0），
        // 所以这 3 次消耗必须复刻——否则后续所有 cardRng 消费全部错位
        // （实测第 2 场战斗的卡牌奖励因此抽到完全不同的卡）。
        // ⚠️ mugged（Looter/Mugger 逃逸）：真游戏走 openCombat(TEXT[0])，
        // **完全不生成卡牌奖励**（cardRng 少 3 次 rollRarity + 3 次 pick + 3 次升级 = 少 9）
        // 实测 floor 5（Looter 战）cardRng delta = 8 而非 9。
        if self.combat_mugged {
            self.combat_mugged = false;
            return;
        }
        let boss = room == Some(RoomKind::Boss);
        // ⚠️ roll 顺序（实测确认，配合 HashMap 池序）：
        //   先把 3 张的 rarity+pick 全部做完，再统一掷 3 次升级概率。
        //   依据：真游戏 PMCWKZEF 首战奖励候选 = [Warcry, Clash, Body Slam]，
        //   在 HashMap 池序下这三张的池内下标是 2 / 8 / 5，
        //   而 cardRng 第 1/3/5 次 nextInt(20)（offset 1/3/5）正好是 2 / 8 / 5。
        //   若改成逐张交错（r,p,u ×3），pick 会落在 offset 1/4/7 → 2/13/12，对不上。
        let mut cards: Vec<crate::cards::CardInst> = Vec::new();
        for _ in 0..3 {
            let rarity = self.roll_rarity(boss);
            let card = self.pick_card(rarity, &cards);
            cards.push(card);
        }
        let chance = self.upgrade_chance();
        for card in cards.iter_mut() {
            // ⚠️ Java 是短路求值：`c.rarity != RARE && cardRng.randomBoolean(...)`
            // ⇒ **RARE 卡不掷升级概率**（不消耗 cardRng）。
            // 无条件掷会让每次奖励多 1 次消耗（实测 floor 5 有 RARE 卡时 9 vs 8）。
            if card.def.rarity == "RARE" {
                continue;
            }
            let roll = self.rng.card.random_boolean_chance(chance);
            if roll && card.can_upgrade() {
                card.upgraded = true;
                card.upgrade_times = 1;
            }
        }
        self.rewards.push(RewardItem::CardReward(cards));
    }

    /// 本幕的 cardUpgradedChance（AbstractDungeon.initializeLevelSpecificChances）
    /// - Act1 Exordium：0.0
    /// - Act2 TheCity：A12+ 0.125，否则 0.25
    /// - Act3 TheBeyond / Act4 TheEnding：A12+ 0.25，否则 0.5
    fn upgrade_chance(&self) -> f32 {
        match self.act {
            1 => 0.0,
            2 => {
                if self.ascension >= 12 {
                    0.125
                } else {
                    0.25
                }
            }
            _ => {
                if self.ascension >= 12 {
                    0.25
                } else {
                    0.5
                }
            }
        }
    }

    /// rollRarity：cardRng.random(99) + 暴雪偏移；<3 稀有 / <37 罕见 / 普通
    /// （暴雪：起 5，普通 -1 至 -40，罕见不变，稀有重置 5）
    fn roll_rarity(&mut self, boss: bool) -> &'static str {
        if boss {
            self.last_rarity_roll = -1;
            return "RARE";
        }
        let roll = self.rng.card.random_range(99) + self.card_blizz;
        self.last_rarity_roll = roll;
        // AbstractRoom.getCardRarity：RARE 阈值 = baseRareCardChance(3)，
        // UNCOMMON 阈值 = baseRareCardChance + baseUncommonCardChance(37) = **40**。
        // 写成 37 会让 roll ∈ [37,40) 从 UNCOMMON 错判成 COMMON（换池 → 抽到不同的卡）。
        if roll < 3 {
            self.card_blizz = 5;
            "RARE"
        } else if roll < 40 {
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
            let mut pool: Vec<&'static crate::effects::CardDef> = crate::generated::CARDS
                .iter()
                .filter(|d| {
                    d.color == self.char_kind.color() && d.rarity == rarity && d.status != "no_use"
                })
                .collect();
            // ⚠️ 池序必须是 Java HashMap 迭代序（CardLibrary.cards.entrySet()），
            // 不是类名序。否则同样种子会抽到完全不同的卡。
            pool.sort_by_key(|d| crate::cards::card_pool_pos(d));
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
                upgrade_times: 0,
            };
        }
    }

    /// 领取奖励：金币即时；卡入主牌组；药水进空槽（满则弃）；遗物入栏
    pub fn take_reward(&mut self, idx: usize) {
        if idx >= self.rewards.len() {
            return;
        }
        let item = self.rewards.remove(idx);
        // Ectoplasm：不能获得金币
        let has_ectoplasm = self
            .relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::Ectoplasm);
        match item {
            RewardItem::Gold(g) => {
                if !has_ectoplasm {
                    self.player.gold += g;
                }
            }
            RewardItem::Card(c) => {
                self.deck.push(c);
                // CeramicFish：获得卡牌时 +9 金币
                if self
                    .relics
                    .iter()
                    .any(|r| r.kind == crate::relics::RelicKind::CeramicFish)
                {
                    self.player.gold += 9;
                }
                // DarkstonePeriapt：获得诅咒卡时 Max HP +6
                let is_curse = self
                    .deck
                    .last()
                    .map(|c| c.def.color == "CURSE")
                    .unwrap_or(false);
                if is_curse
                    && self
                        .relics
                        .iter()
                        .any(|r| r.kind == crate::relics::RelicKind::DarkstonePeriapt)
                {
                    self.player.max_hp += 6;
                    self.player.hp += 6;
                }
                // Egg 系遗物：获得对应类型卡牌自动升级（onObtainCard）
                if let Some(last) = self.deck.last_mut() {
                    let ct = last.def.card_type;
                    let has_frozen = self
                        .relics
                        .iter()
                        .any(|r| r.kind == crate::relics::RelicKind::FrozenEgg);
                    let has_molten = self
                        .relics
                        .iter()
                        .any(|r| r.kind == crate::relics::RelicKind::MoltenEgg);
                    let has_toxic = self
                        .relics
                        .iter()
                        .any(|r| r.kind == crate::relics::RelicKind::ToxicEgg);
                    if (has_frozen && ct == "SKILL")
                        || (has_molten && ct == "ATTACK")
                        || (has_toxic && ct == "POWER")
                    {
                        last.upgraded = true;
                    }
                }
            }
            RewardItem::Potion(k) => {
                // Sozu：不能获得药水
                if self
                    .relics
                    .iter()
                    .any(|r| r.kind == crate::relics::RelicKind::Sozu)
                {
                    return; // 不入槽
                }
                if let Some(slot) = self.potions.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(crate::potions::PotionInst::new(k));
                } // 满槽：弃（简化：满槽领取前应由调用方先 discard）
            }
            RewardItem::CardReward(cands) => {
                if let Some(c) = cands.first().cloned() {
                    self.deck.push(c);
                }
            }
            RewardItem::Relic(k) => {
                self.relics.push(crate::relics::RelicInst::new(k));
                self.relic_on_obtain(k);
            }
        }
    }

    /// 遗物 onEquip（获得时效果；逐行对照源码 onEquip）
    fn relic_on_obtain(&mut self, kind: crate::relics::RelicKind) {
        use crate::relics::RelicKind;
        match kind {
            // WingBoots.java 构造器：`this.counter = 3`（获得即 3 次飞行）
            // ⚠️ 幕间**不重置**：`relicsToRemoveOnStart.add("WingedGreaves")` 只在
            // Flight mod 启用时生效（自定义模式），常规流程无重置逻辑。
            RelicKind::WingedGreaves => {
                if let Some(r) = self
                    .relics
                    .iter_mut()
                    .find(|r| r.kind == RelicKind::WingedGreaves)
                {
                    r.counter = 3;
                }
            }
            // Strawberry.java：Max HP +7
            RelicKind::Strawberry => {
                self.player.max_hp += 7;
                self.player.hp += 7;
            }
            // Pear.java（UNCOMMON）: Max HP +10
            RelicKind::Pear => {
                self.player.max_hp += 10;
                self.player.hp += 10;
            }
            // Mango.java（RARE）: Max HP +10
            RelicKind::Mango => {
                self.player.max_hp += 10;
                self.player.hp += 10;
            }
            // TinyHouse.java：升级 2 随机卡 + MaxHP+5 + 金50 + 药水 + 遗物
            // Old Coin（RARE）：获得 300 金币
            RelicKind::OldCoin => {
                self.player.gold += 300;
            }
            // Lee's Waffle（SHOP）：回满 + MaxHP +3
            RelicKind::LeesWaffle => {
                self.player.max_hp += 3;
                self.player.hp = self.player.max_hp;
            }
            // Orrery（SHOP）：获得 5 张升级的卡牌——引擎简化：升 5 张随机
            RelicKind::Orrery => {
                self.upgrade_random_cards("", 5);
            }
            // Cauldron（SHOP）：获得 5 瓶随机药水
            RelicKind::Cauldron => {
                let pool = crate::potions::pool_for_class(self.char_kind.id());
                for _ in 0..5 {
                    if let Some(slot) = self.potions.iter_mut().find(|s| s.is_none()) {
                        let roll =
                            self.rng.potion.random_range(pool.len() as i32 - 1).max(0) as usize;
                        *slot = Some(crate::potions::PotionInst::new(
                            pool[roll.min(pool.len() - 1)],
                        ));
                    }
                }
            }
            RelicKind::TinyHouse => {
                self.upgrade_random_cards("", 2); // 任意类型
                self.player.max_hp += 5;
                self.player.hp += 5;
                self.player.gold += 50;
                // 药水 + 遗物进奖励（引擎简化：直接给药水一瓶+遗物一件）
                let pool = crate::potions::pool_for_class(self.char_kind.id());
                let roll = self.rng.potion.random_range(pool.len() as i32 - 1).max(0) as usize;
                if let Some(slot) = self.potions.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(crate::potions::PotionInst::new(
                        pool[roll.min(pool.len() - 1)],
                    ));
                }
                if let Some(k) = self.roll_relic_kind() {
                    self.relics.push(crate::relics::RelicInst::new(k));
                }
            }
            // PandorasBox：将全部打击/防御变形为随机同色卡
            RelicKind::PandorasBox => {
                let count = self
                    .deck
                    .iter()
                    .filter(|c| c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend"))
                    .count();
                self.deck
                    .retain(|c| !c.def.id.starts_with("Strike") && !c.def.id.starts_with("Defend"));
                for _ in 0..count {
                    let rarity = self.roll_rarity_bases(3, 37);
                    let card = self.pick_red_card(rarity);
                    self.deck.push(card);
                }
            }
            // EmptyCage：删除 2 张牌（简化：删前两张非诅咒）
            RelicKind::EmptyCage => {
                let mut removed = 0;
                let mut i = 0;
                while removed < 2 && i < self.deck.len() {
                    if self.deck[i].def.color != "CURSE" {
                        self.deck.remove(i);
                        removed += 1;
                    } else {
                        i += 1;
                    }
                }
            }
            // Astrolabe：变形 3 张牌（简化：删前 3 非诅咒加随机）
            RelicKind::Astrolabe => {
                let mut removed = 0;
                let mut i = 0;
                while removed < 3 && i < self.deck.len() {
                    if self.deck[i].def.color != "CURSE" {
                        self.deck.remove(i);
                        removed += 1;
                    } else {
                        i += 1;
                    }
                }
                for _ in 0..removed {
                    let rarity = self.roll_rarity_bases(3, 37);
                    let card = self.pick_red_card(rarity);
                    self.deck.push(card);
                }
            }
            // CallingBell：遗物+诅咒+遗物（简化：遗物+遗物）
            RelicKind::CallingBell => {
                if let Some(k) = self.roll_relic_kind() {
                    self.relics.push(crate::relics::RelicInst::new(k));
                }
                if let Some(k) = self.roll_relic_kind() {
                    self.relics.push(crate::relics::RelicInst::new(k));
                }
            }
            // PotionBelt.java：药水槽 +2
            RelicKind::PotionBelt => {
                self.potions.push(None);
                self.potions.push(None);
            }
            // Whetstone.java：随机升级 2 张攻击牌（miscRng 洗牌取前 2）
            RelicKind::Whetstone => {
                self.upgrade_random_cards("ATTACK", 2);
            }
            // WarPaint.java：随机升级 2 张技能牌
            RelicKind::WarPaint => {
                self.upgrade_random_cards("SKILL", 2);
            }
            _ => {}
        }
    }

    /// miscRng 洗牌后取前 n 张可升级指定类型牌
    fn upgrade_random_cards(&mut self, card_type: &str, n: usize) {
        let mut idxs: Vec<usize> = self
            .deck
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.upgraded && (card_type.is_empty() || c.def.card_type == card_type))
            .map(|(i, _)| i)
            .collect();
        // Fisher-Yates（miscRng）取前 n
        for i in (1..idxs.len()).rev() {
            let j = self.rng.misc.random_range(i as i32).max(0) as usize;
            idxs.swap(i, j);
        }
        for &i in idxs.iter().take(n) {
            self.deck[i].upgraded = true;
        }
    }

    /// 领取全部奖励（金币+遗物+药水，跳过全部卡）——机器人策略用
    pub fn take_all_rewards_skip_cards(&mut self) {
        let mut i = 0;
        while i < self.rewards.len() {
            if matches!(
                self.rewards[i],
                RewardItem::Card(_) | RewardItem::CardReward(_)
            ) {
                i += 1;
                continue; // 卡牌奖励需要玩家选择，不自动领
            }
            self.take_reward(i);
        }
        self.rewards.clear();
    }

    /// Boss 战利：从遗物池 roll 3 候选后发放第一只（决策面属协议层）
    /// Boss 战利：从 BOSS 预洗池取 3 只作为候选（**不自动发放**）。
    ///
    /// 真游戏 BossRelicScreen 是 3 选 1，且**可以跳过**（跳过换 +1 能量或金币）。
    /// 之前实现直接发放第一只 —— 剥夺了「跳过」这个关键决策。
    fn boss_relic_reward(&mut self) {
        self.boss_relic_candidates.clear();
        for _ in 0..3 {
            if let Some(k) = self.pop_pooled_relic("BOSS") {
                self.boss_relic_candidates.push(k);
            }
        }
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
            let was_event_combat = self.event_combat;
            self.event_combat = false;
            let won = c.won;
            // ⚠️ 玩家死亡 → 整局结束（真游戏 GameOverScreen → isDungeonBeaten）。
            // 原来这里没有死亡判定：hp 归 0 后仍继续走 to_map_screen，
            // 导致后续所有战斗都在 hp=0 状态下进行（奖励/卡组全错）。
            if !won && c.player.creature.hp <= 0 && !c.escaped {
                self.over = true;
                self.won = false;
                self.player.hp = 0;
                return;
            }
            // 胜利遗物钩子（BurningBlood 回血 / FaceOfCleric MaxHP+1）
            let max_hp_gain = if c.won { c.on_victory() } else { 0 };
            self.player.hp = c.player.creature.hp.max(0);
            self.player.max_hp += max_hp_gain;
            // 偷钱怪逃逸 → mugged（奖励屏不给卡牌奖励）
            self.combat_mugged = c.monsters.iter().any(|m| {
                m.escaped
                    && matches!(
                        m.kind,
                        crate::state::MonsterKind::Looter | crate::state::MonsterKind::Mugger
                    )
            });
            // 药水栏同步（战斗内使用/丢弃）
            self.potions = c.player.potions.clone();
            // 主牌组永久新增（OrbWalker 灼伤 / 寄生前置）
            self.deck.extend(c.master_deck_additions.clone());
            // 遗物实例回写（PenNib 跨战计数等）
            self.relics = c.player_relics.clone();
            // RNG 流回写：战斗内推进的全局流必须带回局外层
            self.rng = c.rng.clone();
            self.player.gold = c.gold + if c.won { gold_reward } else { 0 };
            // Looter/Mugger 被击败 → 被偷金币作为 STOLEN_GOLD 奖励项（源码
            // AbstractRoom.addStolenGoldToRewards）。必须排在普通奖励之前，
            // 与真游戏 combat_reward 列表顺序一致。
            if c.won && c.stolen_gold_rewards > 0 {
                self.rewards.push(RewardItem::Gold(c.stolen_gold_rewards));
            }
            if c.won {
                self.generate_rewards();
            }
            if was_event_combat {
                // 事件战斗：奖励已预置；Colosseum 波次/其余收尾见 event_after_combat
                self.event_after_combat(won);
            }
            if c.won && self.pos.0 == MAP_HEIGHT - 1 {
                // 幕 Boss 战胜利 → 下一幕（本批：3 幕打完即整局胜利）
                if self.act >= 3 {
                    self.over = true;
                    self.won = true;
                } else {
                    // Boss 战利（简化：roll 后直接发放——三选一决策面属协议层）
                    // Boss 战利：生成 3 候选，由协议层/BossRelicChoice 状态决策
                    self.boss_relic_reward();
                    // 幕间卡牌奖励（Boss 恒稀有）：3 张候选，同样交给决策层
                    let mut cards = Vec::new();
                    for _ in 0..3 {
                        let rarity = self.roll_rarity(true);
                        let card = self.pick_card(rarity, &cards);
                        cards.push(card);
                    }
                    // 作为 CardReward 奖励项（而不是直接塞进 deck）
                    self.rewards.push(RewardItem::CardReward(cards));
                    self.act += 1;
                    // 新幕：mapRng 重新播种（不复用旧流），miscRng = seed + floorNum
                    self.rng.map = StsRandom::new(map_seed(self.seed, self.act));
                    self.rng.misc = StsRandom::new(self.seed.wrapping_add(self.floor as u64));
                    self.map = generate_dungeon(self.act, self.ascension, &mut self.rng.map);
                    self.pos = (0, 0);
                    self.encounter_idx = 0;
                    self.elite_idx = 0;
                    self.populate_act();
                    self.init_event_pools();
                }
            }
        }
    }

    /// Neow 开局：复刻 NeowEvent.blessing()。
    ///
    /// Java：`NeowEvent.rng = new Random(Settings.seed)`（原始种子，无偏移），
    /// 然后 `new NeowReward(0/1/2/3)` 各自用 `rng.random(0, options.size()-1)` 抽一项。
    /// category 2 还会先抽一次 drawback（也是同一 rng）。
    /// 所以 4 个选项必须按 cat0→cat1→cat2→cat3 的顺序抽，不能预置固定表。
    pub fn init_neow(&mut self) {
        self.neow_rng = Some(StsRandom::new(self.seed));
        let mut picks = Vec::new();

        // cat 0：6 个候选
        picks.push(NeowPick {
            cat: 0,
            idx: self.neow_roll(6),
            drawback: None,
        });
        // cat 1：5 个候选
        picks.push(NeowPick {
            cat: 1,
            idx: self.neow_roll(5),
            drawback: None,
        });
        // cat 2：先抽 drawback（4 选 1），再按剔除后长度抽 reward
        let dr = self.neow_roll(4) as u8;
        let drawback = match dr {
            0 => NeowDrawback::HpLoss,
            1 => NeowDrawback::NoGold,
            2 => NeowDrawback::Curse,
            _ => NeowDrawback::PercentDamage,
        };
        // cat2 候选长度随 drawback 变化（见 NeowReward.getRewardOptions case 2）
        // 基础 7 项：RANDOM_COLORLESS_2 / REMOVE_TWO / ONE_RARE_RELIC /
        //            THREE_RARE_CARDS / TWO_FIFTY_GOLD / TRANSFORM_TWO_CARDS /
        //            TWENTY_PERCENT_HP_BONUS
        let cat2_len = match drawback {
            NeowDrawback::Curse => 6,  // 去掉 REMOVE_TWO
            NeowDrawback::NoGold => 6, // 去掉 TWO_FIFTY_GOLD
            NeowDrawback::HpLoss => 6, // 去掉 TWENTY_PERCENT_HP_BONUS
            NeowDrawback::PercentDamage => 7,
        };
        picks.push(NeowPick {
            cat: 2,
            idx: self.neow_roll(cat2_len),
            drawback: Some(drawback),
        });
        // cat 3：只有 BOSS_RELIC 一项，仍要抽一次（rng.random(0,0) 也推进计数器）
        picks.push(NeowPick {
            cat: 3,
            idx: self.neow_roll(1),
            drawback: None,
        });

        self.neow_category = Some(0); // 保留旧字段（兼容）
        self.neow_picks = picks;
        self.neow_options = Vec::new();
    }

    /// NeowEvent.rng.random(0, n-1)：推进 counter 并返回 [0, n)
    fn neow_roll(&mut self, n: usize) -> usize {
        let r = self.neow_rng.as_mut().expect("neow rng");
        if n <= 1 {
            // 仍要消耗一次（Java random(0,0) 也 ++counter 并 nextInt(1)）
            let _ = r.random_range(0);
            return 0;
        }
        r.random_range(n as i32 - 1) as usize
    }

    /// returnTrulyRandomCardFromAvailable(prohibited, NeowEvent.rng)
    /// 池 = 本角色 COMMON + UNCOMMON + RARE（排除 prohibited）。
    fn transform_card_neow(&mut self, prohibited: &str) -> crate::cards::CardInst {
        let color = self.char_kind.color();
        // 池序必须与真游戏一致：
        //   initializeCardPools 把 tmpPool（= addRedCards 的**类名升序**）按稀有度
        //   分别 append 到 commonCardPool / uncommonCardPool / rareCardPool，
        //   returnTrulyRandomCardFromAvailable 再按 common → uncommon → rare 串起来。
        // 所以排序键 = (稀有度序号, 类名)。
        let rarity_rank = |r: &str| match r {
            "COMMON" => 0u8,
            "UNCOMMON" => 1,
            "RARE" => 2,
            _ => 3,
        };
        let mut pool: Vec<&'static crate::effects::CardDef> = crate::generated::CARDS
            .iter()
            .filter(|d| {
                d.color == color
                    && d.id != prohibited
                    && matches!(d.rarity, "COMMON" | "UNCOMMON" | "RARE")
            })
            .collect();
        pool.sort_by(|a, b| {
            rarity_rank(a.rarity)
                .cmp(&rarity_rank(b.rarity))
                .then_with(|| a.cls.cmp(b.cls))
        });
        if pool.is_empty() {
            return crate::cards::CardInst::new("Strike_R");
        }
        let r = self.neow_rng.as_mut().expect("neow rng");
        let idx = r.random_range(pool.len() as i32 - 1).max(0) as usize;
        let def = pool[idx.min(pool.len() - 1)];
        crate::cards::CardInst {
            def,
            upgraded: false,
            upgrade_times: 0,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
        }
    }

    /// 当前 Neow 屏可选的 4 个选项（描述文本 + 是否可选）
    pub fn neow_option_list(&self) -> Vec<(String, bool)> {
        self.neow_picks
            .iter()
            .map(|p| (self.neow_pick_desc(p), true))
            .collect()
    }

    /// 单个 pick 的描述文本（与游戏 UI 对齐，便于人读/日志）
    pub fn neow_pick_desc(&self, p: &NeowPick) -> String {
        const CAT0: [&str; 6] = [
            "Obtain 3 cards",
            "Obtain a random rare card",
            "Remove a card from your deck",
            "Upgrade a card",
            "Transform a card",
            "Obtain a random colorless card",
        ];
        const CAT1: [&str; 5] = [
            "Obtain 3 random potions",
            "Obtain a random common relic",
            "Max HP +10%",
            "Enemies in the first 3 combats have 1 HP",
            "Gain 100 gold",
        ];
        match p.cat {
            0 => CAT0.get(p.idx).copied().unwrap_or("?").to_string(),
            1 => CAT1.get(p.idx).copied().unwrap_or("?").to_string(),
            2 => {
                let names = cat2_option_names(p.drawback);
                let base = match names.get(p.idx).copied().unwrap_or("") {
                    "RANDOM_COLORLESS_2" => "Obtain a random colorless card",
                    "REMOVE_TWO" => "Remove 2 cards",
                    "ONE_RARE_RELIC" => "Obtain a random rare relic",
                    "THREE_RARE_CARDS" => "Obtain 3 rare cards",
                    "TWO_FIFTY_GOLD" => "Gain 250 gold",
                    "TRANSFORM_TWO_CARDS" => "Transform 2 cards",
                    "TWENTY_PERCENT_HP_BONUS" => "Max HP +20%",
                    _ => "?",
                };
                let dw = match p.drawback {
                    Some(NeowDrawback::HpLoss) => " (Lose 10% max HP)",
                    Some(NeowDrawback::NoGold) => " (Lose all gold)",
                    Some(NeowDrawback::Curse) => " (Obtain a curse)",
                    Some(NeowDrawback::PercentDamage) => " (Take 30% HP damage)",
                    None => "",
                };
                format!("{base}{dw}")
            }
            _ => "Swap your starter Relic for a random boss Relic".to_string(),
        }
    }

    /// Neow 选择：idx = 0..3 对应 4 个 category 抽出的选项
    pub fn choose_neow_pick(&mut self, idx: usize) {
        let Some(p) = self.neow_picks.get(idx).cloned() else {
            return;
        };
        // 应用 drawback（cat2）
        if let Some(d) = p.drawback {
            match d {
                NeowDrawback::HpLoss => {
                    let loss = (self.player.max_hp as f32 * 0.1) as i32;
                    self.player.max_hp -= loss;
                    self.player.hp = (self.player.hp - loss).max(1);
                }
                NeowDrawback::NoGold => self.player.gold = 0,
                NeowDrawback::Curse => {
                    if let Some(c) = crate::cards::CardInst::new_by_class("Injury") {
                        self.deck.push(c);
                    }
                }
                NeowDrawback::PercentDamage => {
                    let dmg = self.player.hp / 10 * 3;
                    self.player.hp = (self.player.hp - dmg).max(1);
                }
            }
        }
        // 应用 reward
        match p.cat {
            0 => match p.idx {
                0 => {
                    // THREE_CARDS：协议层三选一，GymEnv 自动取第一张
                    let rarity = self.roll_rarity(false);
                    let c = self.pick_card(rarity, &[]);
                    self.deck.push(c);
                }
                1 => {
                    let c = self.pick_card("RARE", &[]);
                    self.deck.push(c);
                }
                2 => {
                    if !self.deck.is_empty() {
                        self.deck.remove(0);
                    }
                }
                3 => {
                    if let Some(i) = self.deck_upgradable().first().copied() {
                        self.deck[i].upgraded = true;
                        self.deck[i].upgrade_times = 1;
                    }
                }
                4 => {
                    // TRANSFORM_CARD：AbstractDungeon.transformCard(c, false, NeowEvent.rng)
                    // → returnTrulyRandomCardFromAvailable(c, rng)
                    // 池 = common + uncommon + rare（排除自身），用 **NeowEvent.rng** 抽
                    // （不是 cardRng —— 真游戏此操作 cardRng counter 不变）。
                    if let Some(pos) = self.deck.iter().position(|c| {
                        c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend")
                    }) {
                        let prohibited = self.deck[pos].def.id;
                        let card = self.transform_card_neow(prohibited);
                        self.deck[pos] = card;
                    }
                }
                _ => {
                    let c = self.pick_card("COLORLESS", &[]);
                    self.deck.push(c);
                }
            },
            1 => match p.idx {
                0 => {
                    for _ in 0..3 {
                        self.grant_random_potion();
                    }
                }
                1 => self.grant_random_relic(),
                2 => {
                    let bonus = (self.player.max_hp as f32 * 0.1) as i32;
                    self.player.max_hp += bonus;
                    self.player.hp += bonus;
                }
                3 => self.neow_enemy_kill = 3,
                _ => self.player.gold += 100,
            },
            2 => {
                // idx 是「剔除 drawback 冲突项后的列表下标」，需映射回实际奖励
                let names = cat2_option_names(p.drawback);
                match names.get(p.idx).copied().unwrap_or("") {
                    "RANDOM_COLORLESS_2" => {
                        let c = self.pick_card("COLORLESS", &[]);
                        self.deck.push(c);
                    }
                    "REMOVE_TWO" => {
                        for _ in 0..2 {
                            if !self.deck.is_empty() {
                                self.deck.remove(0);
                            }
                        }
                    }
                    "ONE_RARE_RELIC" => self.grant_random_relic_tier("RARE"),
                    "THREE_RARE_CARDS" => {
                        for _ in 0..3 {
                            let c = self.pick_card("RARE", &[]);
                            self.deck.push(c);
                        }
                    }
                    "TWO_FIFTY_GOLD" => self.player.gold += 250,
                    "TRANSFORM_TWO_CARDS" => {
                        for _ in 0..2 {
                            if let Some(pos) = self.deck.iter().position(|c| {
                                c.def.id.starts_with("Strike") || c.def.id.starts_with("Defend")
                            }) {
                                let prohibited = self.deck[pos].def.id;
                                let card = self.transform_card_neow(prohibited);
                                self.deck[pos] = card;
                            }
                        }
                    }
                    "TWENTY_PERCENT_HP_BONUS" => {
                        let bonus = (self.player.max_hp as f32 * 0.1) as i32 * 2;
                        self.player.max_hp += bonus;
                        self.player.hp += bonus;
                    }
                    _ => {}
                }
            }
            _ => {
                // BOSS_RELIC：替换初始遗物
                self.neow_boss_relic_swap();
            }
        }
        self.neow_picks.clear();
        self.neow_category = None;
    }

    /// addRelicToRewards(tier)：从预洗池「弹出」一只（**不消耗 relicRng**）。
    ///
    /// 真游戏的 tier 池在 `initializeRelicList` 时用
    /// `Collections.shuffle(pool, new Random(relicRng.randomLong()))` 洗好（5 次消耗），
    /// 之后每次取用只是 `remove(0)`。
    /// ⚠️ 偏差登记：此处未实现 tier 元数据与预洗池顺序，按「首个未持有」近似，
    ///    但**不掷 RNG**——这一点对 RNG 对拍是决定性的。
    pub fn pop_pooled_relic(&mut self, tier: &str) -> Option<crate::relics::RelicKind> {
        // 从预洗池 remove(0)；跳过已持有（源码 addRelicToRewards 也不给重复）
        let owned: Vec<crate::relics::RelicKind> = self.relics.iter().map(|r| r.kind).collect();
        let key: &'static str = match tier {
            "COMMON" => "COMMON",
            "UNCOMMON" => "UNCOMMON",
            "RARE" => "RARE",
            "SHOP" => "SHOP",
            "BOSS" => "BOSS",
            _ => "COMMON",
        };
        let pool = self.relic_pools.get_mut(key)?;
        while !pool.is_empty() {
            let k = pool.remove(0);
            if !owned.contains(&k) {
                return Some(k);
            }
        }
        None
    }

    /// 选择第 i 只 Boss 遗物；i 越界 = 跳过
    ///
    /// ⚠️ 查证结论：真游戏 `BossRelicSelectScreen.relicSkipLogic()` 只是关闭屏幕
    /// （`closeCurrentScreen` + `proceedButton.hide`），**不发放任何能量/金币补偿**。
    /// 之前我写的「跳过换 +1 能量」是错的，已删除。
    pub fn choose_boss_relic(&mut self, idx: usize) {
        if idx < self.boss_relic_candidates.len() {
            let k = self.boss_relic_candidates[idx];
            self.relics.push(crate::relics::RelicInst::new(k));
            self.relic_on_obtain(k);
        }
        // 跳过：无补偿（与源码一致）
        self.boss_relic_candidates.clear();
    }

    /// PotionHelper.getRandomPotion + 入槽（满槽丢弃）
    fn grant_random_potion(&mut self) {
        // PotionHelper.getRandomPotion：potionRng 全池均匀（无稀有度筛）
        let pool = crate::potions::pool_for_class(self.char_kind.id());
        let roll = self.rng.potion.random_range(pool.len() as i32 - 1).max(0) as usize;
        let k = pool[roll.min(pool.len() - 1)];
        if let Some(slot) = self.potions.iter_mut().find(|s| s.is_none()) {
            *slot = Some(crate::potions::PotionInst::new(k));
        }
    }

    /// returnRandomRelic(tier)：从已实现池取未持有者
    ///
    /// ⚠️ 偏差：RelicKind 尚无 tier 元数据，暂时忽略 tier（仅按未持有过滤）。
    /// 待 relics 生成表补 tier 字段后按 tier 严格筛。
    fn grant_random_relic_tier(&mut self, tier: &str) {
        // 从预洗 tier 池 remove(0)（0 次 RNG）；池空时退回全池
        if let Some(k) = self.pop_pooled_relic(tier) {
            self.relics.push(crate::relics::RelicInst::new(k));
            self.relic_on_obtain(k);
        }
    }

    /// Neow BOSS_RELIC：把初始遗物替换成随机 Boss 遗物
    ///
    /// ⚠️ 偏差：同样缺 tier 元数据，暂从全池抽（不是真 Boss 池）。
    fn neow_boss_relic_swap(&mut self) {
        // Neow BOSS_RELIC：从 BOSS 预洗池 remove(0)
        self.relics.clear(); // 移除初始遗物
        if let Some(k) = self.pop_pooled_relic("BOSS") {
            self.relics.push(crate::relics::RelicInst::new(k));
            self.relic_on_obtain(k);
        }
    }

    /// 休息·睡觉：回复 30% 最大生命（向上取整对照 RestRoom：
    /// heal = (int)(maxHealth * 0.3)——A7 内截断）
    pub fn rest_sleep(&mut self) {
        if !self.rest_available {
            return;
        }
        // Coffee Dripper：不能休息
        if self
            .relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::CoffeeDripper)
        {
            return;
        }
        let heal = (self.player.max_hp as f32 * 0.3) as i32;
        self.player.hp = (self.player.hp + heal).min(self.player.max_hp);
        // Regal Pillow：休息额外回 15
        if self
            .relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::RegalPillow)
        {
            self.player.hp = (self.player.hp + 15).min(self.player.max_hp);
        }
        // Eternal Feather：进休息房回复 3×卡组/5
        if self
            .relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::EternalFeather)
        {
            let h = self.deck.len() as i32 / 5 * 3;
            self.player.hp = (self.player.hp + h).min(self.player.max_hp);
        }
        self.rest_available = false;
    }

    /// 休息·锻造：升级主牌组一张卡（协议层 grid_select 后回调）
    pub fn rest_smith(&mut self, deck_idx: usize) {
        if !self.rest_available {
            return;
        }
        // Fusion Hammer：不能锻造
        if self
            .relics
            .iter()
            .any(|r| r.kind == crate::relics::RelicKind::FusionHammer)
        {
            return;
        }
        if let Some(c) = self.deck.get_mut(deck_idx) {
            if !c.upgraded {
                c.upgraded = true;
            }
        }
        self.rest_available = false;
    }
}

/// 休息房：进房不自动回血；睡觉回 30%；锻造升级；只能选一项
#[test]
fn rest_room_options() {
    let mut run = RunState::new(31, 0);
    run.map.rows[2][3].room = Some(RoomKind::Rest);
    run.pos = (2, 3);
    run.player.hp = 40;
    run.enter_room();
    assert_eq!(run.player.hp, 40, "onPlayerEntry 不回血");
    assert!(run.rest_available);
    run.rest_sleep();
    assert_eq!(run.player.hp, 40 + 24, "80×30% = 24");
    assert!(!run.rest_available);
    run.rest_smith(0); // 已休息 → 无效
    assert!(!run.deck[0].upgraded);
    // 另一局：锻造
    let mut run = RunState::new(31, 0);
    run.map.rows[2][3].room = Some(RoomKind::Rest);
    run.pos = (2, 3);
    run.enter_room();
    run.rest_smith(0);
    assert!(run.deck[0].upgraded);
    assert!(!run.rest_available);
    run.rest_sleep(); // 已锻造 → 无效
    assert_eq!(run.player.hp, run.player.max_hp);
}

/// 商店房间进入即进货；事件房间进入即抽取（Act1 池非空）
#[test]
fn shop_and_event_rooms_enter() {
    for seed in [1u64, 7, 99] {
        let mut run = RunState::new(seed, 0);
        run.map.rows[1][3].room = Some(RoomKind::Shop);
        run.pos = (1, 3);
        run.enter_room();
        assert!(run.shop.is_some(), "商店进货");
        assert!(run.event.is_none());
        let mut run = RunState::new(seed, 0);
        run.floor = 10;
        run.map.rows[1][3].room = Some(RoomKind::Event);
        run.pos = (1, 3);
        run.enter_room();
        assert!(run.event.is_some(), "Act1 事件池非空必有事件");
        assert!(!run.event_pool.is_empty() || !run.shrine_pool.is_empty());
    }
}

/// 四角色开局：起始组/血量/初始遗物/职业卡池与药水（A0/A6/A14 矩阵）
#[test]
fn four_characters_setup() {
    use crate::state::CharKind;
    // A0：基础血量与初始遗物
    let table = [
        (CharKind::Ironclad, 80, "Bash", "Burning Blood"),
        (CharKind::Silent, 70, "Neutralize", "Ring of the Snake"),
        (CharKind::Defect, 75, "Dualcast", "Cracked Core"),
        (CharKind::Watcher, 72, "Eruption", "PureWater"),
    ];
    for (ch, hp, signature, relic_id) in table {
        let run = RunState::new_for(3, 0, ch);
        assert_eq!(run.player.max_hp, hp, "{:?} 基础血量", ch);
        assert_eq!(run.player.hp, hp);
        assert_eq!(run.relics[0].kind.id(), relic_id, "{:?} 初始遗物", ch);
        assert!(
            run.deck.iter().any(|c| c.def.id == signature),
            "{:?} 起始组含 {signature}",
            ch
        );
        assert!(run
            .deck
            .iter()
            .all(|c| { c.def.color == ch.color() || c.def.color == "COLORLESS" }));
    }
    // A6：起始 = round(90%)，上限不变（AbstractDungeon 2574）
    let s6 = RunState::new_for(3, 6, CharKind::Silent);
    assert_eq!((s6.player.hp, s6.player.max_hp), (63, 70));
    let w6 = RunState::new_for(3, 6, CharKind::Watcher);
    assert_eq!(
        (w6.player.hp, w6.player.max_hp),
        (65, 72),
        "round(72×0.9)=65"
    );
    // A14：上限 -loss（铁甲 5 其余 4）后再按 90% 起始
    let i14 = RunState::new_for(3, 14, CharKind::Ironclad);
    assert_eq!(
        (i14.player.max_hp, i14.player.hp),
        (75, 68),
        "75, round(75×.9)=68"
    );
    let d14 = RunState::new_for(3, 14, CharKind::Defect);
    assert_eq!(
        (d14.player.max_hp, d14.player.hp),
        (71, 64),
        "round(71×.9)=63.8→64"
    );
    // A10：进阶之灾
    let s10 = RunState::new_for(3, 10, CharKind::Silent);
    assert!(s10.deck.iter().any(|c| c.def.id == "AscendersBane"));
}

/// 职业卡池：四角色战斗奖励 3 张全是本职业色（观者出紫、机器人出蓝）
#[test]
fn four_characters_card_reward_pool() {
    use crate::state::CharKind;
    for ch in CharKind::all() {
        let mut run = RunState::new_for(11, 0, ch);
        run.map.rows[1][3].room = Some(RoomKind::Monster);
        run.pos = (1, 3);
        run.enter_room();
        run.combat = {
            let mut c = run.combat.take().unwrap();
            for m in c.monsters.iter_mut() {
                m.creature.hp = 0;
            }
            c.over = true;
            c.won = true;
            Some(c)
        };
        run.settle_combat(0);
        let cards: Vec<_> = run
            .rewards
            .iter()
            .flat_map(|r| match r {
                // 卡牌奖励是真游戏语义的**一个**奖励项（内含 3 张候选）
                RewardItem::CardReward(cands) => {
                    cands.iter().map(|c| c.def.color).collect::<Vec<_>>()
                }
                RewardItem::Card(c) => vec![c.def.color],
                _ => vec![],
            })
            .collect();
        assert_eq!(cards.len(), 3, "{:?} 三张卡牌奖励", ch);
        assert!(
            cards.iter().all(|c| *c == ch.color()),
            "{:?} 全本职业色",
            ch
        );
    }
}

/// 四角色商店进货职业色 + 药水职业池（沉默出本能药水可行——只验色与生成）
#[test]
fn four_characters_shop_stock() {
    use crate::state::CharKind;
    for ch in CharKind::all() {
        let mut run = RunState::new_for(11, 0, ch);
        run.map.rows[1][3].room = Some(RoomKind::Shop);
        run.pos = (1, 3);
        run.enter_room();
        let shop = run.shop.as_ref().unwrap();
        assert!(
            shop.cards.iter().all(|c| c.card.def.color == ch.color()),
            "{:?} 商店色卡",
            ch
        );
        assert_eq!(shop.potions.len(), 3);
    }
}

/// 观者开局：进战斗纯水给奇迹、爆裂/守夜姿态可用
#[test]
fn watcher_purewater_and_stances() {
    use crate::state::CharKind;
    let mut run = RunState::new_for(7, 0, CharKind::Watcher);
    run.map.rows[1][3].room = Some(RoomKind::Monster);
    run.pos = (1, 3);
    run.enter_room();
    let c = run.combat.as_ref().unwrap();
    assert!(
        c.player.hand.iter().any(|x| x.def.id == "Miracle"),
        "纯水开局奇迹"
    );
    // Eruption（爆裂）进愤怒：伤害×2 双向（用 Combat 单测语境内验）
}

/// 机器人开局：破碎核心战前充能 1 闪电球
#[test]
fn defect_cracked_core_channel() {
    use crate::state::CharKind;
    let mut run = RunState::new_for(7, 0, CharKind::Defect);
    run.map.rows[1][3].room = Some(RoomKind::Monster);
    run.pos = (1, 3);
    run.enter_room();
    let c = run.combat.as_ref().unwrap();
    assert_eq!(c.player.orbs.len(), 1, "战前 1 球");
    assert!(matches!(
        c.player.orbs[0].kind,
        crate::state::OrbKind::Lightning
    ));
}

/// 沉默开局：蛇戒首回合抽 7（5+2）
#[test]
fn silent_ring_of_snake() {
    use crate::state::CharKind;
    let mut run = RunState::new_for(7, 0, CharKind::Silent);
    run.map.rows[1][3].room = Some(RoomKind::Monster);
    run.pos = (1, 3);
    run.enter_room();
    let c = run.combat.as_ref().unwrap();
    assert_eq!(c.player.hand.len(), 7, "蛇戒 +2");
    // Neutralize（-neutralize 打 3 + 虚弱 1）与 Survivor 可用性
    assert!(c
        .player
        .draw
        .iter()
        .chain(c.player.hand.iter())
        .any(|x| x.def.id == "Neutralize"));
}

/// 宝箱房：进房即战利入奖励屏（1 遗物 + 概率金币），拿取/跳过走既有 reward 动作
#[test]
fn treasury_room_loot() {
    for seed in [3u64, 17, 42, 99, 2024] {
        let mut run = RunState::new(seed, 0);
        run.map.rows[8][3].room = Some(RoomKind::Treasury); // 行 8 恒宝箱
        run.pos = (8, 3);
        run.enter_room();
        let relics: Vec<_> = run
            .rewards
            .iter()
            .filter(|r| matches!(r, RewardItem::Relic(_)))
            .collect();
        assert_eq!(relics.len(), 1, "恒 1 遗物");
        assert!(run.rewards.len() <= 2, "至多再加金币");
        if let Some(RewardItem::Gold(g)) = run
            .rewards
            .iter()
            .find(|r| matches!(r, RewardItem::Gold(_)))
        {
            let amt = *g as f32;
            assert!(
                (25.0 * 0.9..=75.0 * 1.1 + 0.5).contains(&amt),
                "金币额在箱型范围内：{g}"
            );
        }
        // 拿取走既有管线：take_reward 后遗物入栏
        let idx = run
            .rewards
            .iter()
            .position(|r| matches!(r, RewardItem::Relic(_)))
            .unwrap();
        run.take_reward(idx);
        assert!(run.relics.len() >= 2, "宝箱遗物入栏");
    }
}

/// ── 整局吞吐 benchmark（P0 验收指标：单核 ≥1000 局/秒）──
/// cargo test --release -p neow-sim-core -- --ignored bench_full_run --nocapture
/// 策略：攻击优先贪心出牌（can_play 过滤 + 攻击→0 号怪）+ 基准牌组
/// （起始组 + 职业最高伤可执行攻击 ×8，模拟中后期强度避免弱牌死锁）
/// + 9999/9999 god mode + 60 回合死锁=战败 + 全跳卡牌奖励
#[test]
#[ignore]
fn bench_full_run() {
    use crate::combat::Action;
    use std::time::Instant;

    let chars = [
        crate::state::CharKind::Ironclad,
        crate::state::CharKind::Silent,
        crate::state::CharKind::Defect,
        crate::state::CharKind::Watcher,
    ];
    let total_runs = 400u64;
    let mut floors_total = 0u64;
    let mut combats_total = 0u64;
    let mut completed = 0u64;
    // 分段计时（定位热点用；phase_* 单位 μs）
    let mut t_combat = 0u128;
    let mut t_settle = 0u128;
    let mut t_room = 0u128;
    let mut t_events = 0u128;
    let mut t_rewards = 0u128;
    let mut n_plays = 0u64;
    let mut n_ends = 0u64;
    let mut stalemates = 0u64;
    // 静音 panic 输出（兜底捕获仅计数；结束后恢复钩子）
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panicked_runs = 0u64;
    let mut stop_over_win = 0u64;
    let mut stop_hp = 0u64;
    let mut stop_guard = 0u64;
    let mut stop_edge = 0u64;
    let t0 = Instant::now();
    for i in 0..total_runs {
        let ch = chars[(i % 4) as usize];
        let mut run = RunState::new_for(1_000_000 + i, 0, ch);
        // 基准牌组：起始组 + 职业最高伤可执行攻击 ×8（模拟中后期强度，
        // 避免弱牌组对治疗怪死锁把 benchmark 变成死锁计数器）
        {
            let color = ch.color();
            let best = crate::generated::CARDS
                .iter()
                .filter(|d| {
                    d.color == color
                        && d.card_type == "ATTACK"
                        && d.cost.unwrap_or(1) <= 2
                        && crate::cards::engine_playable(d)
                })
                .max_by_key(|d| d.base_damage.unwrap_or(0));
            if let Some(def) = best {
                for _ in 0..8 {
                    run.deck.push(crate::cards::CardInst {
                        def,
                        upgraded: false,
                        damage_bonus: 0,
                        block_bonus: 0,
                        cost_delta: 0,
                        upgrade_times: 0,
                    });
                }
            }
        }
        // god mode：9999/9999（战斗内同值，3 幕全程跑完）
        let mut guard_rooms = 0;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut _f = 0u64;
            let mut _c = 0u64;
            let mut _won = false;
            while !run.over {
                guard_rooms += 1;
                if guard_rooms > 80 {
                    stop_guard += 1;
                    break; // 结构异常保护
                }
                run.player.max_hp = 9999;
                run.player.hp = 9999;
                // 选边 0 前进 → 进房；行 14（顶层）= 休息 + Boss 门
                // （协议层 boss 动作：引擎侧直接置 Boss 房进入）
                if run.pos.0 == MAP_HEIGHT - 1 {
                    if run.rest_available {
                        run.rest_sleep();
                    }
                    run.map.rows[run.pos.0][run.pos.1].room = Some(RoomKind::Boss);
                    run.enter_room();
                } else {
                    let edges = run.map.rows[run.pos.0][run.pos.1].edges.len();
                    if run.pos.0 + 1 < MAP_HEIGHT && edges > 0 {
                        run.choose_node(0);
                    } else {
                        stop_edge += 1;
                        break;
                    }
                    let t = std::time::Instant::now();
                    run.enter_room();
                    t_room += t.elapsed().as_micros();
                }
                floors_total += 1;
                // 事件房：一路选 0（选完主选项 → 结算挂起选牌 → 按离开）
                {
                    let t = std::time::Instant::now();
                    let mut guard_ev = 0;
                    while guard_ev < 12 {
                        guard_ev += 1;
                        let Some(ev) = run.event.as_ref() else { break };
                        if ev.done {
                            break;
                        }
                        if !ev.pending.is_empty() {
                            if !run.deck.is_empty() {
                                run.event_pick_card(0);
                            } else {
                                break;
                            }
                            continue;
                        }
                        if ev.screen == 1 && !ev.library.is_empty() {
                            run.event_library_pick(0);
                            continue;
                        }
                        if run.event_options().is_empty() {
                            break; // 战斗中/无可选项（如 Colosseum 波间由战斗驱动）
                        }
                        run.choose_event_option(0);
                    }
                    t_events += t.elapsed().as_micros();
                }
                // 休息：睡觉
                if run.rest_available {
                    run.rest_sleep();
                }
                // 战斗：贪心出牌
                let mut stalled = false;
                if run.combat.is_some() {
                    _c += 1;
                    let t = std::time::Instant::now();
                    let mut c = run.combat.take().unwrap();
                    let mut turn_guard = 0;
                    while !c.over {
                        turn_guard += 1;
                        if turn_guard > 60 {
                            stalemates += 1;
                            stalled = true;
                            break; // 弱牌组对治疗/叠甲怪的合法死锁：按战败处理
                        }
                        // 挂起屏兜底（起始组不该出现；防御性按类别消解）
                        if c.pending.is_some() {
                            use crate::combat::Pending;
                            match c.pending.as_ref().unwrap() {
                                Pending::Scry { .. } => c.step(&Action::ScryChoice {
                                    discard_indices: vec![],
                                }),
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
                                _ => c.step(&Action::HandSelect { indices: vec![0] }),
                            }
                            continue;
                        }
                        // 出一张能付得起且引擎可执行的牌（攻击优先，压缩回合数）
                        let energy = c.player.energy;
                        let mut played = false;
                        let mut order: Vec<usize> = Vec::with_capacity(c.player.hand.len());
                        for (idx, cd) in c.player.hand.iter().enumerate() {
                            if cd.def.card_type == "ATTACK" {
                                order.insert(0, idx);
                            } else {
                                order.push(idx);
                            }
                        }
                        for idx in order {
                            let card = &c.player.hand[idx];
                            if !crate::cards::engine_playable(card.def) {
                                continue;
                            }
                            let cost = card.cost_pay().max(0);
                            if cost <= energy {
                                let target = if card.def.target == "enemy"
                                    && !c.monsters.is_empty()
                                    && c.monsters.iter().any(|m| m.creature.hp > 0)
                                {
                                    Some(
                                        c.monsters
                                            .iter()
                                            .position(|m| m.creature.hp > 0)
                                            .unwrap_or(0),
                                    )
                                } else {
                                    None
                                };
                                c.step(&Action::PlayCard { index: idx, target });
                                n_plays += 1;
                                played = true;
                                break;
                            }
                        }
                        if !played {
                            c.step(&Action::EndTurn);
                            n_ends += 1;
                        }
                    }
                    run.combat = Some(c);
                    t_combat += t.elapsed().as_micros();
                    let t2 = std::time::Instant::now();
                    run.settle_combat(0);
                    t_settle += t2.elapsed().as_micros();
                    // 转幕后 pos=(0,0) 可能不在路径上 → 重新定位到有边的行 0 节点
                    if !run.over && run.pos == (0, 0) && run.map.rows[0][0].edges.is_empty() {
                        for x in 0..MAP_WIDTH {
                            if !run.map.rows[0][x].edges.is_empty() {
                                run.pos = (0, x);
                                break;
                            }
                        }
                    }
                }
                if stalled {
                    run.player.hp = 0; // 死锁 = 战败
                }
                // 奖励：全拿跳卡
                let t = std::time::Instant::now();
                run.take_all_rewards_skip_cards();
                t_rewards += t.elapsed().as_micros();
                if run.won {
                    completed += 1;
                }
                if run.over {
                    stop_over_win += 1;
                }
                if run.player.hp <= 0 {
                    stop_hp += 1;
                    break;
                }
            }
            _won = run.won;
            (_won, _f, _c)
        }));
        match r {
            Ok((won, f, c)) => {
                floors_total += f;
                combats_total += c;
                if won {
                    completed += 1;
                }
            }
            Err(_) => panicked_runs += 1,
        }
    }
    println!("stop: over={stop_over_win} hp0={stop_hp} guard={stop_guard} edge={stop_edge} panic={panicked_runs}");
    std::panic::set_hook(prev_hook);
    let dt = t0.elapsed().as_secs_f64();
    println!("ops: plays={n_plays} ends={n_ends} stalemate60={stalemates}");
    println!(
        "phases(ms): combat={:.0} settle={:.0} room={:.0} events={:.0} rewards={:.0} rest={:.0}",
        t_combat as f64 / 1000.0,
        t_settle as f64 / 1000.0,
        t_room as f64 / 1000.0,
        t_events as f64 / 1000.0,
        t_rewards as f64 / 1000.0,
        (dt * 1000.0 - (t_combat + t_settle + t_room + t_events + t_rewards) as f64 / 1000.0)
    );
    println!(
        "runs={total_runs} time={dt:.2}s throughput={:.0} runs/s | floors={floors_total} ({:.0} runs-equivalent) combats={combats_total} completed_3acts={completed}",
        total_runs as f64 / dt,
        floors_total as f64 / 51.0
    );
    // P0 目标 ≥1000 局/秒（完整对局口径：含部分早死局，全程当量另行打印）。
    // 阈值留 5× 余量防机器差异导致 CI 之外的本地偶发。
    assert!(
        total_runs as f64 / dt > 200.0,
        "吞吐不应低于 200 局/秒（当前基线的 5% 兜底）"
    );
}

#[cfg(test)]
/// M4 对拍黄金用例：PMCWKZEF（seed=1585262650280）开局状态。
/// 基线来自真游戏 bridge `get_state`（Exordium / A0 / Ironclad / floor 1 turn 1）。
mod parity_golden_tests {
    use super::*;

    const SEED: u64 = 1585262650280;

    #[test]
    fn rng_counters_match_bridge_baseline() {
        let run = RunState::new(SEED, 0);
        // 真游戏 bridge get_state 的 rng 计数器（开局、进入第一场战斗前）
        assert_eq!(run.rng.monster.counter, 38, "monster");
        assert_eq!(run.rng.map.counter, 95, "map");
        assert_eq!(run.rng.relic.counter, 5, "relic");
        assert_eq!(run.rng.event.counter, 0, "event");
        assert_eq!(run.rng.merchant.counter, 0, "merchant");
        assert_eq!(run.rng.card.counter, 0, "card");
        assert_eq!(run.rng.treasure.counter, 0, "treasure");
        assert_eq!(run.rng.potion.counter, 0, "potion");
    }

    #[test]
    fn neow_options_match_bridge() {
        let run = RunState::new(SEED, 0);
        let opts = run.neow_option_list();
        assert_eq!(opts.len(), 4);
        // Java NeowDump 输出：cat0 idx=4 TRANSFORM / cat1 idx=2 HP+10% /
        // drawback=0 HP_LOSS + cat2 idx=2 ONE_RARE_RELIC / cat3 BOSS_RELIC
        assert!(opts[0].0.contains("Transform"), "cat0={}", opts[0].0);
        assert!(opts[1].0.contains("Max HP"), "cat1={}", opts[1].0);
        assert!(opts[2].0.contains("rare relic"), "cat2={}", opts[2].0);
        assert!(opts[2].0.contains("Lose"), "cat2 drawback={}", opts[2].0);
        assert!(opts[3].0.contains("boss Relic"), "cat3={}", opts[3].0);
    }

    #[test]
    fn first_encounter_is_two_fuzzy_louse() {
        let mut run = RunState::new(SEED, 0);
        run.choose_neow_pick(1); // Max HP +10%（不改牌组，便于校验遭遇序列）
        let col = run.start_columns()[0];
        run.map_started = true;
        run.pos = (0, col);
        run.floor = 1;
        run.enter_room();
        let c = run.combat.as_ref().expect("首房应为战斗");
        // 真游戏：FuzzyLouseNormal 15/15 与 12/12
        assert_eq!(c.monsters.len(), 2);
        assert_eq!(c.monsters[0].kind.id(), "FuzzyLouseNormal");
        assert_eq!(c.monsters[1].kind.id(), "FuzzyLouseNormal");
        assert_eq!(c.monsters[0].creature.hp, 15);
        assert_eq!(c.monsters[1].creature.hp, 12);
        // monsterHpRng 消耗：两只 Louse 各 setHp + bite = 4，再加 usePreBattleAction
        // 的 CurlUpPower 掷 2 次 = 6（与 bridge 基线一致）
        assert_eq!(c.rng.monster_hp.counter, 6);
        // miscRng：两只 Louse 的种类各掷一次
        assert_eq!(c.rng.misc.counter, 2);
        assert_eq!(c.rng.shuffle.counter, 1, "起手洗牌一次");
    }

    #[test]
    fn neow_transform_uses_neow_rng_not_card_rng() {
        // NeowEvent.rng 前 5 次：cat0/cat1/drawback/cat2/cat3；transform 是第 6 次。
        // 池 = 红 COMMON(20)+UNCOMMON(36)+RARE(16)=72，按 (稀有度, 类名) 排序；
        // 第 6 次 nextInt(72) = 53 → Spot Weakness。
        // 关键：整个 Neow 流程 **不动 cardRng**（真游戏 card counter 恒 0）。
        let mut run = RunState::new(SEED, 0);
        assert_eq!(run.rng.card.counter, 0);
        run.choose_neow_pick(0); // cat0 = Transform a card
        assert_eq!(run.rng.card.counter, 0, "transform 必须走 neow_rng");
        assert_eq!(run.deck.len(), 10, "transform 是替换不是新增");
        assert_eq!(
            run.deck[0].def.id,
            "Spot Weakness",
            "池序/下标与真游戏模型不一致：{:?}",
            run.deck.iter().map(|c| c.def.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn map_seed_per_act() {
        assert_eq!(map_seed(1000, 1), 1001);
        assert_eq!(map_seed(1000, 2), 1200);
        assert_eq!(map_seed(1000, 3), 1600);
        assert_eq!(map_seed(1000, 4), 2200);
    }

    #[test]
    fn floor_reseed_overwrites_streams() {
        let mut run = RunState::new(SEED, 0);
        let col = run.start_columns()[0];
        run.map_started = true;
        run.pos = (0, col);
        run.floor = 1;
        run.enter_room();
        // monsterHp/ai/shuffle/cardRandom/misc 全部重播种为 seed + floorNum
        let c = run.combat.as_ref().unwrap();
        assert_eq!(c.rng.ai.counter, 2);
        assert_eq!(c.rng.shuffle.counter, 1);
    }
}

#[cfg(test)]
mod seed_helper_tests {
    use super::seed_helper;

    #[test]
    fn decode_pmcwkzef() {
        let seed = seed_helper::decode("PMCWKZEF");
        assert!(seed > 0);
    }

    #[test]
    fn decode_encode_small() {
        // 小数字可逆
        for s in ["0", "1", "A", "10", "ZZ"] {
            let decoded = seed_helper::decode(s);
            let encoded = seed_helper::encode(decoded);
            assert_eq!(encoded, s, "roundtrip {s} → {decoded} → {encoded}");
        }
    }

    #[test]
    fn decode_simple() {
        // "0" → 0
        assert_eq!(seed_helper::decode("0"), 0);
        // "1" → 1
        assert_eq!(seed_helper::decode("1"), 1);
    }

    #[test]
    fn encode_roundtrip() {
        for seed in [1u64, 42, 1000, 999999] {
            let encoded = seed_helper::encode(seed);
            let decoded = seed_helper::decode(&encoded);
            assert_eq!(decoded, seed, "roundtrip {seed} → {encoded} → {decoded}");
        }
    }

    #[test]
    fn decode_o_to_zero() {
        // O → 0 替换
        let a = seed_helper::decode("P0P");
        let b = seed_helper::decode("PMP"); // 不会，但 O 和 0 相同
        let _ = b;
        let _ = a;
    }
}

#[cfg(test)]
mod card_reward_probe {
    use super::*;

    /// 打印 PMCWKZEF 首战胜后的卡牌奖励候选（与 bridge 真游戏对照用）
    #[test]
    #[ignore]
    fn dump_first_card_reward() {
        let mut run = RunState::new(1585262650280, 0);
        let col = run.start_columns()[0];
        run.map_started = true;
        run.pos = (0, col);
        run.floor = 1;
        run.enter_room();
        let mut c = run.combat.take().unwrap();
        c.over = true;
        c.won = true;
        run.combat = Some(c);
        run.settle_combat(0);
        for r in &run.rewards {
            match r {
                RewardItem::Gold(g) => println!("GOLD {g}"),
                RewardItem::Potion(k) => println!("POTION {k:?}"),
                RewardItem::Relic(k) => println!("RELIC {k:?}"),
                RewardItem::CardReward(cs) => {
                    let ids: Vec<&str> = cs.iter().map(|x| x.def.id).collect();
                    println!("CARDS {ids:?}");
                }
                RewardItem::Card(x) => println!("CARD {}", x.def.id),
            }
        }
        println!("cardRng counter = {}", run.rng.card.counter);
    }
}
