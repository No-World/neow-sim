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

/// Act 1 遭遇池（Exordium）。
/// ⚠️ 已知偏差：Large Slime / Lots of Slimes 未实现（AcidSlime_L/SpikeSlime_L
/// 待补），暂从池中移除并在此登记——对拍前必须恢复（docs/parity-spec.md 偏差表）
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
        // Large Slime (2.0) / Lots of Slimes (1.0)：未实现，偏差登记
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

    /// 进入当前节点的房间（战斗类生成 Combat；非战斗类本批为占位）。
    /// 行 14 房间显示为休息房，但真游戏在 Boss 前强制打 Boss——本批以
    /// choose_node 消耗 Boss 哨兵边后由测试显式调 boss 战（简化，登记偏差）
    pub fn enter_room(&mut self) {
        let (y, x) = self.pos;
        let room = self.map.rows[y][x].room;
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
                let mut c = Combat::new_with(self.seed, self.ascension, kinds);
                // 局外状态带入（HP/金币）
                c.player.creature.hp = self.player.hp;
                c.player.creature.max_hp = self.player.max_hp;
                c.gold = self.player.gold;
                self.combat = Some(c);
            }
            Some(RoomKind::Rest) => {
                // 睡觉：回 30% 最大生命（RestRoom.onUseCard 简化——锻造后续批）
                let heal = (self.player.max_hp as f32 * 0.3) as i32;
                self.player.hp = (self.player.hp + heal).min(self.player.max_hp);
            }
            Some(RoomKind::Boss) => {
                let mut c = Combat::new_with(self.seed, self.ascension, self.boss);
                c.player.creature.hp = self.player.hp;
                c.player.creature.max_hp = self.player.max_hp;
                c.gold = self.player.gold;
                self.combat = Some(c);
            }
            _ => {
                // 事件/商店/宝箱：占位（下批实现）
            }
        }
    }

    /// 战斗结束回收（结算 HP/金币；胜利推进——幕末转幕）
    pub fn settle_combat(&mut self, gold_reward: i32) {
        if let Some(c) = self.combat.take() {
            self.player.hp = c.player.creature.hp.max(0);
            self.player.gold = c.gold + if c.won { gold_reward } else { 0 };
            if c.won && self.pos.0 == MAP_HEIGHT - 1 {
                // 幕 Boss 战胜利 → 下一幕（本批：3 幕打完即整局胜利）
                if self.act >= 3 {
                    self.over = true;
                    self.won = true;
                } else {
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
