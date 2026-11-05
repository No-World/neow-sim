//! 遭遇构造链 dump：对每个遭遇名输出 (怪物列表, misc 计数, monster_hp 计数)。
//!
//! 与 `tools/audit_encounter_runtime.py`（真游戏构造链的 Python 模型）对拍，
//! 用于锁定「候选全构造 / 无放回抽取 / 每次 1 次 miscRng」等流语义。
use neow_sim_core::run::RunState;

fn main() {
    let names: Vec<String> = std::env::args().skip(1).collect();
    let names = if names.is_empty() {
        DEFAULT.to_vec()
    } else {
        names.iter().map(|s| s.as_str()).collect()
    };
    for name in names {
        let mut run = RunState::new(1585262650280, 0);
        let m0 = run.rng.misc.counter;
        let h0 = run.rng.monster_hp.counter;
        let states = run.resolve_encounter(name);
        println!(
            "{}\t{:?}\tmisc={}\thp={}",
            name,
            states.iter().map(|s| s.kind).collect::<Vec<_>>(),
            run.rng.misc.counter - m0,
            run.rng.monster_hp.counter - h0
        );
    }
}

const DEFAULT: &[&str] = &[
    "2 Louse",
    "3 Louse",
    "Small Slimes",
    "Lots of Slimes",
    "Large Slime",
    "Gremlin Gang",
    "Exordium Thugs",
    "Exordium Wildlife",
    "2 Fungi Beasts",
    "Gremlin Leader",
    "3 Shapes",
    "4 Shapes",
    "3 Byrds",
    "4 Byrds",
    "Chosen and Byrds",
    "Sentry and Sphere",
    "Snecko and Mystics",
    "Centurion and Healer",
    "Shelled Parasite and Fungi",
    "Cultist and Chosen",
    "3 Cultists",
    "2 Thieves",
    "Slavers",
    "Masked Bandits",
    "Flame Bruiser 1 Orb",
    "Flame Bruiser 2 Orb",
    "Jaw Worm Horde",
    "3 Darklings",
    "Colosseum Nobs",
    "Colosseum Slavers",
];
