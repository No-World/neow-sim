//! M2 卡牌（数值来自 neow-sim/data/cards.json 提取表）
//! 铁甲战士起手：5×打击、4×防御、1×痛击（+A10 起飞升者之灾）

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CardKind {
    Strike,
    Defend,
    Bash,
    AscendersBane,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardInst {
    pub kind: CardKind,
    pub upgraded: bool,
}

impl CardInst {
    pub const fn new(kind: CardKind) -> Self {
        Self {
            kind,
            upgraded: false,
        }
    }

    pub const fn upgraded(kind: CardKind) -> Self {
        Self {
            kind,
            upgraded: true,
        }
    }

    /// 费用（M2 起手卡全部固定费）
    pub fn cost(&self) -> i32 {
        match self.kind {
            CardKind::Strike | CardKind::Defend => 1,
            CardKind::Bash => 2,
            CardKind::AscendersBane => -2, // 不可打出
        }
    }

    pub fn playable(&self) -> bool {
        !matches!(self.kind, CardKind::AscendersBane)
    }

    /// baseDamage（Strike 6/+3、Bash 8/+2）
    pub fn base_damage(&self) -> Option<i32> {
        match self.kind {
            CardKind::Strike => Some(6 + self.upgraded as i32 * 3),
            CardKind::Bash => Some(8 + self.upgraded as i32 * 2),
            _ => None,
        }
    }

    /// baseBlock（Defend 5/+3）
    pub fn base_block(&self) -> Option<i32> {
        match self.kind {
            CardKind::Defend => Some(5 + self.upgraded as i32 * 3),
            _ => None,
        }
    }

    /// baseMagicNumber（Bash 的易伤 2/+1）
    pub fn magic(&self) -> Option<i32> {
        match self.kind {
            CardKind::Bash => Some(2 + self.upgraded as i32),
            _ => None,
        }
    }
}

pub fn ironclad_starter(ascension: i32) -> Vec<CardInst> {
    let mut deck = vec![CardInst::new(CardKind::Strike); 5];
    deck.extend([CardInst::new(CardKind::Defend); 4]);
    deck.push(CardInst::new(CardKind::Bash));
    if ascension >= 10 {
        deck.push(CardInst::new(CardKind::AscendersBane));
    }
    deck
}
