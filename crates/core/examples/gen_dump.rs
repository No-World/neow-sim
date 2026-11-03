//! 生成链对拍 dump：对每个 seed 输出「地图边结构 + 房间类型 + 遭遇序列 + Boss + Neow 抽签 + RNG 计数器」。
//!
//! 配合 `.tools/javagen/GenDump.java`（直接调用真游戏 jar）逐行 diff。
//!
//! 用法:
//!   cargo run --example gen_dump -- 1 42 12345
//!
//! 输出为行式文本（便于 diff，无需 serde）。

use neow_sim_core::run::{RoomKind, RunState, MAP_HEIGHT, MAP_WIDTH};

fn room_letter(r: RoomKind) -> char {
    match r {
        RoomKind::Monster => 'M',
        RoomKind::Elite => 'E',
        RoomKind::Rest => 'R',
        RoomKind::Shop => 'S',
        RoomKind::Event => '?',
        RoomKind::Treasury => 'T',
        RoomKind::Boss => 'B',
    }
}

fn main() {
    let seeds: Vec<u64> = std::env::args()
        .skip(1)
        .map(|s| s.parse::<u64>().expect("seed must be u64"))
        .collect();
    if seeds.is_empty() {
        eprintln!("usage: gen_dump <seed> [seed...]");
        std::process::exit(2);
    }

    for seed in seeds {
        let run = RunState::new(seed, 0);
        println!("seed {seed}");

        // 地图边结构
        for y in 0..MAP_HEIGHT {
            for x in 0..MAP_WIDTH {
                let n = &run.map.rows[y][x];
                if n.edges.is_empty() {
                    continue;
                }
                let dst: Vec<String> = n.edges.iter().map(|e| e.to_string()).collect();
                println!("map {y} {x} > {}", dst.join(","));
            }
        }

        // 房间类型
        for y in 0..MAP_HEIGHT {
            for x in 0..MAP_WIDTH {
                if let Some(r) = run.map.rows[y][x].room {
                    println!("room {y} {x} {}", room_letter(r));
                }
            }
        }

        // 遭遇序列（populateMonsterList 产物）
        for (i, name) in run.encounter_list.iter().enumerate() {
            println!("enc monster {i} {name}");
        }
        for (i, name) in run.elite_names.iter().enumerate() {
            println!("enc elite {i} {name}");
        }
        println!("boss {}", run.boss_name);

        // Neow 4 选项
        for (i, (desc, _)) in run.neow_option_list().iter().enumerate() {
            println!("neow {i} {desc}");
        }

        // RNG 计数器（锁住整条生成链的消耗数）
        println!(
            "rng monster={} map={} relic={} event={} merchant={} card={} treasure={} potion={}",
            run.rng.monster.counter,
            run.rng.map.counter,
            run.rng.relic.counter,
            run.rng.event.counter,
            run.rng.merchant.counter,
            run.rng.card.counter,
            run.rng.treasure.counter,
            run.rng.potion.counter,
        );
        println!("end");
    }
}
