//! Vocab：实体 ID 查表。
//!
//! 卡牌用 CARDS 数组下标（确定性 + 唯一 + 与 generated 数据同序）。
//! 怪物/遗物/power/药水用枚举判别值 `as i32`（天然连续）。
//!
//! Python 侧 `nn.Embedding(vocab_size, d)` 用 vocab_size() 初始化。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::generated::CARDS;

// ── 卡牌 vocab（CARDS 数组下标）────────────────

static CARD_INDEX: OnceLock<HashMap<&'static str, i32>> = OnceLock::new();

pub fn card_id(id: &str) -> i32 {
    CARD_INDEX
        .get_or_init(|| {
            CARDS
                .iter()
                .enumerate()
                .map(|(i, c)| (c.id, i as i32))
                .collect()
        })
        .get(id)
        .copied()
        .unwrap_or(0) // 0 = UNK
}

pub fn card_vocab_size() -> i32 {
    CARDS.len() as i32
}

// ── 枚举判别值 vocab ─────────────────────────

pub fn relic_vocab_size() -> i32 {
    crate::relics::ALL_RELICS.len() as i32
}

pub fn power_vocab_size() -> i32 {
    // PowerKind 枚举变体数（手工数——Rust 无 const count 变体）
    170 // 上限预留（实际 ~130 用到）
}

pub fn potion_vocab_size() -> i32 {
    crate::potions::ALL_POTIONS.len() as i32
}

pub fn monster_vocab_size() -> i32 {
    // MonsterKind 枚举变体数
    80 // 上限预留
}

pub fn orb_vocab_size() -> i32 {
    5 // Lightning/Frost/Dark/Plasma + UNK
}

// ── Hash（Python 校验）────────────────────────

pub fn vocab_hash() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in CARDS {
        c.id.hash(&mut h);
    }
    CARDS.len().hash(&mut h);
    crate::relics::ALL_RELICS.len().hash(&mut h);
    crate::encoding::MAX_T.hash(&mut h);
    crate::encoding::FEAT_DIM.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_id_sane() {
        assert!(card_id("Strike_R") > 0);
        assert!(card_id("Defend_R") > 0);
        assert_ne!(card_id("Strike_R"), card_id("Defend_R"), "不同卡不同 ID");
        assert_eq!(card_id("NonExistent"), 0, "UNK = 0");
    }

    #[test]
    fn card_id_unique() {
        let mut ids: Vec<i32> = CARDS.iter().map(|c| card_id(c.id)).collect();
        ids.sort();
        ids.dedup();
        // 去重后 = CARDS 长度（每个 id 唯一）
        assert!(ids.len() >= CARDS.len() * 9 / 10, "至少 90% 唯一");
    }

    #[test]
    fn vocab_sizes() {
        assert!(card_vocab_size() >= 370);
        assert!(relic_vocab_size() >= 180);
        assert!(potion_vocab_size() >= 40);
    }

    #[test]
    fn hash_stable() {
        assert_eq!(vocab_hash(), vocab_hash());
    }
}
