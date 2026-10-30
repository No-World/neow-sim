// @generated 目录：由 tools/gen_rust.py 生成
mod cards;
mod chars;
mod monsters;

pub use cards::CARDS;
pub use chars::CHARACTERS;
pub use monsters::MONSTERS;

/// 按 id 查卡牌定义
pub fn find_card(id: &str) -> Option<&'static crate::effects::CardDef> {
    CARDS.iter().find(|c| c.id == id)
}

/// 按 id 查怪物定义
pub fn find_monster(id: &str) -> Option<&'static crate::effects::MonsterDef> {
    MONSTERS.iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{Effect, Expr, Target};

    #[test]
    fn registry_counts() {
        assert!(CARDS.len() >= 370, "卡池全量 {}", CARDS.len());
        assert!(MONSTERS.len() >= 70, "怪物全量 {}", MONSTERS.len());
        assert_eq!(CHARACTERS.len(), 4);
    }

    #[test]
    fn spot_checks() {
        let bash = find_card("Bash").unwrap();
        assert!(matches!(
            bash.effects[0],
            Effect::Damage(Target::Enemy, Expr::DamageBase)
        ));
        let cleave = find_card("Cleave").unwrap();
        assert!(matches!(cleave.effects[0], Effect::DamageAll(_)));
        let clothes = find_card("Clothesline").unwrap();
        assert!(matches!(
            clothes.effects[1],
            Effect::ApplyPower(_, "Weakened", _)
        ));
        // 怒气：伤害 + 复制入弃牌堆
        let anger = find_card("Anger").unwrap();
        assert!(matches!(anger.effects[1], Effect::CopySelfToDiscard));
    }

    #[test]
    fn characters_data() {
        let ic = CHARACTERS.iter().find(|c| c.id == "Ironclad").unwrap();
        assert_eq!(ic.max_hp, Some(80));
        assert_eq!(ic.starting_deck.len(), 10);
        let silent = CHARACTERS.iter().find(|c| c.id == "TheSilent").unwrap();
        assert_eq!(silent.max_hp, Some(70));
        assert_eq!(silent.starting_deck.len(), 12);
    }

    #[test]
    fn jaw_worm_data() {
        let jw = find_monster("JawWorm").unwrap();
        assert!(jw
            .hp
            .iter()
            .any(|h| h.asc == 7 && h.min == Some(42) && h.max == Some(46)));
        assert_eq!(jw.ai_probabilities.len(), 3);
    }
}
