//! 商店（ShopRoom + Merchant + ShopScreen 逐行对照）。
//!
//! 源码对照（反编译 `rooms/ShopRoom.java` / `shop/Merchant.java` / `shop/ShopScreen.java`）：
//! - 进货（Merchant 构造器）：角色色卡 5 张 = 攻击×2 / 技能×2 / 能力×1，
//!   每张 `getCardFromPool(rollRarity(), type, true)`（cardRng）；
//!   同型相邻槽位去重（第 2 攻击 vs 第 1 攻击、第 2 技能 vs 第 1 技能），
//!   去重重掷**连稀有度一起重掷**（while 内重新 rollRarity）；
//!   无色卡 2 张 = UNCOMMON + RARE（colorlessCardPool，cardRng）
//! - 稀有度（rollRarity → ShopRoom.getCardRarity）：cardRng.random(99) + 暴雪偏移，
//!   商店基线 rare 9 / uncommon 37（对照 ShopRoom 构造器 baseRareCardChance=9），
//!   暴雪偏移与战斗奖励共用（起 5，普通 -1 至 -40，稀有重置 5）
//! - 定价（ShopScreen.initCards / initRelics / initPotions）：
//!   卡：基价 50/75/150（AbstractCard.getPrice）× merchantRng.random(0.9,1.1)，(int) 截断；
//!   无色卡再 ×1.2；然后 coloredCards[merchantRng.random(0,4)] 半价
//!   遗物：基价 150/250/300（tier；商店件 150，引擎商店池空 → 源码回退 UNCOMMON 250）
//!   × merchantRng.random(0.95,1.05)，MathUtils.round = (x+0.5) 取整
//!   药水：基价 50/75/100 × merchantRng.random(0.95,1.05) 同上
//! - 删卡服务：75 起，每次 +25（跨商店累计，整局保留）；A16 全场物价 ×1.1
//!   （applyDiscount(1.1, false)，不影响删卡价）
//! - 遗物槽位：i<2 走 rollRelicTier（merchantRng.random(99)：<48 COMMON / <82
//!   UNCOMMON / 其余 RARE）；i==2 走 SHOP 池
//!
//! ⚠️ 残留偏差：
//! - 遗物无 tier 池元数据：tier 骰照掷（保持 merchantRng 消耗序列），
//!   具体遗物从已实现池（IMPLEMENTED_RELICS）中 relicRng 取未持有者
//! - Courier / Membership Card / Smiling Mask 未实现（未入遗物池），折扣与
//!   买后补货不生效
//! - 药水槽满时不可购买（源码可先弃后买，属协议层交互）

use crate::cards::CardInst;
use crate::effects::CardDef;
use crate::generated::CARDS;
use crate::potions::{return_random_potion, PotionKind};
use crate::relics::RelicKind;
use crate::rng::RngStreams;
use crate::run::RunState;

/// AbstractCard.getPrice：COMMON 50 / UNCOMMON 75 / RARE 150
fn card_base_price(rarity: &str) -> i32 {
    match rarity {
        "COMMON" => 50,
        "UNCOMMON" => 75,
        _ => 150,
    }
}

/// AbstractRelic.getPrice（tier 基价；商店件 150）
fn relic_base_price(tier: &str) -> i32 {
    match tier {
        "COMMON" => 150,
        "RARE" => 300,
        _ => 250, // UNCOMMON / SHOP 回退
    }
}

/// AbstractPotion.getPrice：COMMON 50 / UNCOMMON 75 / RARE 100
fn potion_base_price(kind: PotionKind) -> i32 {
    use crate::potions::PotionRarity::*;
    match kind.rarity() {
        Common => 50,
        Uncommon => 75,
        Rare => 100,
    }
}

/// 商店货架单件
#[derive(Clone, Debug)]
pub struct ShopCard {
    pub card: CardInst,
    pub price: i32,
    pub sold: bool,
}

#[derive(Clone, Debug)]
pub struct ShopRelic {
    pub kind: RelicKind,
    pub price: i32,
    pub sold: bool,
}

#[derive(Clone, Debug)]
pub struct ShopPotion {
    pub kind: PotionKind,
    pub price: i32,
    pub sold: bool,
}

/// 一次商店访问的全部货架（ShopScreen.init 产物）
#[derive(Clone, Debug)]
pub struct Shop {
    /// 5 张角色色卡（Merchant 顺序：攻击/攻击/技能/技能/能力）
    pub cards: Vec<ShopCard>,
    /// 2 张无色卡（UNCOMMON、RARE）
    pub colorless: Vec<ShopCard>,
    /// 半价卡（cards 下标）
    pub sale_idx: usize,
    /// 3 件遗物（前 2 tier 骰、第 3 商店池→UNCOMMON 回退）
    pub relics: Vec<ShopRelic>,
    /// 3 瓶药水
    pub potions: Vec<ShopPotion>,
    /// actualPurgeCost（进店快照；购买后 +25 写回 run 层）
    pub purge_cost: i32,
    /// 删卡服务剩余次数（每店一次）
    pub purge_available: bool,
}

/// Java `Random.random(start, end)` = start + random()*(end-start)（random() ∈ [0,1)）
fn jitter(rng: &mut crate::rng::StsRandom, lo: f32, hi: f32) -> f32 {
    lo + rng.random_float() * (hi - lo)
}

impl Shop {
    /// 进货 + 定价。调用方持有 `card_blizz`（暴雪偏移与奖励共用）、
    /// `owned`（已持有遗物）、`purge_cost`（整局累计删卡价）。
    pub fn generate(
        ascension: i32,
        char_kind: crate::state::CharKind,
        card_blizz: &mut i32,
        owned: &[RelicKind],
        mut purge_cost: i32,
        implemented: &[RelicKind],
        rng: &mut RngStreams,
    ) -> Self {
        // ── Merchant：角色色卡 5 张（A A S S P）──
        let types = ["ATTACK", "ATTACK", "SKILL", "SKILL", "POWER"];
        let mut cards: Vec<ShopCard> = Vec::new();
        for (i, ty) in types.iter().enumerate() {
            loop {
                // rollRarity：商店基线 rare 9 / uncommon 37（重掷时连稀有度一起）
                let rarity = roll_rarity_shop(&mut rng.card, card_blizz);
                let card = pick_from_pool(Some(ty), char_kind.color(), rarity, &mut rng.card);
                // 同型相邻去重（仅槽位 1/3 检查上一张）
                if i % 2 == 1 && cards[i - 1].card.def.id == card.def.id {
                    continue;
                }
                cards.push(ShopCard {
                    card,
                    price: 0,
                    sold: false,
                });
                break;
            }
        }
        // 无色卡 2 张：UNCOMMON + RARE
        let mut colorless = Vec::new();
        for rarity in ["UNCOMMON", "RARE"] {
            let card = pick_from_pool(None, "COLORLESS", rarity, &mut rng.card);
            colorless.push(ShopCard {
                card,
                price: 0,
                sold: false,
            });
        }
        // ── initCards 定价：基价 × merchantRng(0.9,1.1)，(int) 截断 ──
        for c in cards.iter_mut() {
            let base = card_base_price(c.card.def.rarity) as f32;
            c.price = (base * jitter(&mut rng.merchant, 0.9, 1.1)) as i32;
        }
        for c in colorless.iter_mut() {
            let base = card_base_price(c.card.def.rarity) as f32 * 1.2;
            c.price = (base * jitter(&mut rng.merchant, 0.9, 1.1)) as i32;
        }
        // 半价卡：merchantRng.random(0, 4)
        let sale_idx = rng.merchant.random_start_end(0, 4).clamp(0, 4) as usize;
        cards[sale_idx].price /= 2;

        // ── initRelics：i<2 tier 骰；i==2 SHOP（池空 → UNCOMMON 回退）──
        let mut relics = Vec::new();
        for i in 0..3 {
            let tier = if i < 2 {
                let roll = rng.merchant.random_start_end(0, 99);
                if roll < 48 {
                    "COMMON"
                } else if roll < 82 {
                    "UNCOMMON"
                } else {
                    "RARE"
                }
            } else {
                "UNCOMMON" // SHOP → returnEndRandomRelicKey 空池回退
            };
            // ⚠️ 偏差：tier 骰已消耗 merchantRng；具体遗物走 relicRng 已实现池
            let kind =
                pick_relic(owned, implemented, &mut rng.relic).unwrap_or(RelicKind::SpiritPoop); // 池空兜底（不再消耗）
            let price = {
                let base = relic_base_price(tier) as f32;
                let j = jitter(&mut rng.merchant, 0.95, 1.05);
                (base * j + 0.5) as i32 // MathUtils.round
            };
            relics.push(ShopRelic {
                kind,
                price,
                sold: false,
            });
        }

        // ── initPotions：3 瓶（returnRandomPotion：potionRng）──
        let pool = crate::potions::pool_for_class(char_kind.id());
        let mut potions = Vec::new();
        for _ in 0..3 {
            let kind = return_random_potion(&pool, false, &mut rng.potion);
            let price = {
                let base = potion_base_price(kind) as f32;
                let j = jitter(&mut rng.merchant, 0.95, 1.05);
                (base * j + 0.5) as i32
            };
            potions.push(ShopPotion {
                kind,
                price,
                sold: false,
            });
        }

        // ── A16：全场物价 ×1.1（MathUtils.round；删卡价不受影响）──
        if ascension >= 16 {
            for c in cards.iter_mut() {
                c.price = ((c.price as f32) * 1.1 + 0.5) as i32;
            }
            for c in colorless.iter_mut() {
                c.price = ((c.price as f32) * 1.1 + 0.5) as i32;
            }
            for r in relics.iter_mut() {
                r.price = ((r.price as f32) * 1.1 + 0.5) as i32;
            }
            for p in potions.iter_mut() {
                p.price = ((p.price as f32) * 1.1 + 0.5) as i32;
            }
        }

        // ── 商店遗物折扣（Courier ×0.8 / Membership ×0.5；叠加取更低）──
        let courier = owned.contains(&RelicKind::TheCourier);
        let membership = owned.contains(&RelicKind::MembershipCard);
        if membership {
            for c in cards.iter_mut() {
                c.price = ((c.price as f32) * 0.5 + 0.5) as i32;
            }
            for c in colorless.iter_mut() {
                c.price = ((c.price as f32) * 0.5 + 0.5) as i32;
            }
            for r in relics.iter_mut() {
                r.price = ((r.price as f32) * 0.5 + 0.5) as i32;
            }
            for p in potions.iter_mut() {
                p.price = ((p.price as f32) * 0.5 + 0.5) as i32;
            }
        } else if courier {
            for c in cards.iter_mut() {
                c.price = ((c.price as f32) * 0.8 + 0.5) as i32;
            }
            for c in colorless.iter_mut() {
                c.price = ((c.price as f32) * 0.8 + 0.5) as i32;
            }
            for r in relics.iter_mut() {
                r.price = ((r.price as f32) * 0.8 + 0.5) as i32;
            }
            for p in potions.iter_mut() {
                p.price = ((p.price as f32) * 0.8 + 0.5) as i32;
            }
        }
        // Smiling Mask：删卡恒 50 金
        if owned.contains(&RelicKind::SmilingMask) {
            purge_cost = 50;
        }

        Shop {
            cards,
            colorless,
            sale_idx,
            relics,
            potions,
            purge_cost,
            purge_available: true,
        }
    }
}

/// rollRarity（商店基线）：roll = cardRng.random(99) + 暴雪偏移；
/// <9 RARE（重置 5）/ <46 UNCOMMON / 其余 COMMON（-1 至 -40）
fn roll_rarity_shop(rng: &mut crate::rng::StsRandom, blizz: &mut i32) -> &'static str {
    let roll = rng.random_range(99) + *blizz;
    if roll < 9 {
        *blizz = 5;
        "RARE"
    } else if roll < 9 + 37 {
        "UNCOMMON"
    } else {
        *blizz = (*blizz - 1).max(-40);
        "COMMON"
    }
}

/// getCardFromPool / getColorlessCardFromPool：
/// CardGroup.getRandomCard(type?, useRng=true) → cardRng.random(size-1) 均匀抽取。
/// POWER 空池逐级上浮（RARE ← UNCOMMON ← COMMON，能力池按稀有度天然非空）。
fn pick_from_pool(
    ty: Option<&str>,
    color: &str,
    rarity: &'static str,
    rng: &mut crate::rng::StsRandom,
) -> CardInst {
    let mut tiers = [rarity, rarity, rarity];
    if ty == Some("POWER") {
        // 能力卡空池上浮链（对照 getCardFromPool）
        tiers = match rarity {
            "COMMON" => ["COMMON", "UNCOMMON", "RARE"],
            "UNCOMMON" => ["UNCOMMON", "RARE", "RARE"],
            _ => ["RARE", "RARE", "RARE"],
        };
    }
    for t in tiers {
        let pool: Vec<&'static CardDef> = CARDS
            .iter()
            .filter(|d| {
                d.color == color
                    && d.rarity == t
                    && d.status != "no_use"
                    && ty.is_none_or(|ty| d.card_type == ty)
            })
            .collect();
        if pool.is_empty() {
            continue;
        }
        let roll = rng.random_range(pool.len() as i32 - 1).max(0) as usize;
        return CardInst {
            def: pool[roll.min(pool.len() - 1)],
            upgraded: false,
            damage_bonus: 0,
            block_bonus: 0,
            cost_delta: 0,
                upgrade_times: 0,
        };
    }
    CardInst::new("Strike_R") // 兜底（防数据空洞）
}

/// 已实现池中取未持有遗物（relicRng.random(size-1)；⚠️ 偏差见模块注释）
fn pick_relic(
    owned: &[RelicKind],
    implemented: &[RelicKind],
    rng: &mut crate::rng::StsRandom,
) -> Option<RelicKind> {
    let pool: Vec<RelicKind> = implemented
        .iter()
        .copied()
        .filter(|k| !owned.contains(k))
        .collect();
    if pool.is_empty() {
        return None;
    }
    let roll = rng.random_range(pool.len() as i32 - 1).max(0) as usize;
    Some(pool[roll.min(pool.len() - 1)])
}

// 商店购买动作（ShopScreen.purchaseCard / StoreRelic.purchaseRelic /
// StorePotion.purchasePotion / purgeCard 逐行对照；返回 false = 金币不足/
// 已售罄/槽位不足）。删卡购买后整局删卡价 +25（purgeCard：
// `actualPurgeCost = purgeCost += 25`）。
impl RunState {
    pub fn shop_buy_card(&mut self, idx: usize) -> bool {
        let Some(shop) = self.shop.as_mut() else {
            return false;
        };
        let Some(slot) = shop.cards.get(idx) else {
            return false;
        };
        if slot.sold || self.player.gold < slot.price {
            return false;
        }
        let price = slot.price;
        let card = slot.card;
        self.player.gold -= price;
        self.deck.push(card);
        self.shop.as_mut().unwrap().cards[idx].sold = true;
        true
    }

    pub fn shop_buy_colorless(&mut self, idx: usize) -> bool {
        let Some(shop) = self.shop.as_mut() else {
            return false;
        };
        let Some(slot) = shop.colorless.get(idx) else {
            return false;
        };
        if slot.sold || self.player.gold < slot.price {
            return false;
        }
        let price = slot.price;
        let card = slot.card;
        self.player.gold -= price;
        self.deck.push(card);
        self.shop.as_mut().unwrap().colorless[idx].sold = true;
        true
    }

    pub fn shop_buy_relic(&mut self, idx: usize) -> bool {
        let Some(shop) = self.shop.as_mut() else {
            return false;
        };
        let Some(slot) = shop.relics.get(idx) else {
            return false;
        };
        if slot.sold || self.player.gold < slot.price {
            return false;
        }
        let price = slot.price;
        let kind = slot.kind;
        self.player.gold -= price;
        self.relics.push(crate::relics::RelicInst::new(kind));
        self.shop.as_mut().unwrap().relics[idx].sold = true;
        true
    }

    /// 药水购买：需有空槽（满槽先 discard 属协议层交互，⚠️ 偏差：不可先弃后买）
    pub fn shop_buy_potion(&mut self, idx: usize) -> bool {
        let Some(shop) = self.shop.as_mut() else {
            return false;
        };
        let Some(slot) = shop.potions.get(idx) else {
            return false;
        };
        if slot.sold || self.player.gold < slot.price {
            return false;
        }
        if !self.potions.iter().any(|p| p.is_none()) {
            return false;
        }
        let price = slot.price;
        let kind = slot.kind;
        self.player.gold -= price;
        if let Some(s) = self.potions.iter_mut().find(|p| p.is_none()) {
            *s = Some(crate::potions::PotionInst::new(kind));
        }
        self.shop.as_mut().unwrap().potions[idx].sold = true;
        true
    }

    /// 删卡服务（每店一次；价取整局累计值，购后 +25）
    pub fn shop_purge(&mut self, deck_idx: usize) -> bool {
        let Some(shop) = self.shop.as_mut() else {
            return false;
        };
        if !shop.purge_available || self.player.gold < shop.purge_cost {
            return false;
        }
        if deck_idx >= self.deck.len() {
            return false;
        }
        let cost = shop.purge_cost;
        self.player.gold -= cost;
        self.deck.remove(deck_idx);
        self.purge_cost += 25;
        self.shop.as_mut().unwrap().purge_available = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{RoomKind, RunState};

    /// 强制进入商店：把当前节点改成 Shop 房后 enter_room
    fn force_shop(seed: u64, ascension: i32) -> RunState {
        let mut run = RunState::new(seed, ascension);
        run.map.rows[1][3].room = Some(RoomKind::Shop);
        run.pos = (1, 3);
        run.enter_room();
        run
    }

    /// 进货形状：5 色卡（攻攻技技能序）+ 2 无色 + 3 遗物 + 3 药水 + 删卡 75
    #[test]
    fn stock_shape() {
        let run = force_shop(7, 0);
        let shop = run.shop.as_ref().expect("商店已生成");
        assert_eq!(shop.cards.len(), 5);
        let types: Vec<&str> = shop.cards.iter().map(|c| c.card.def.card_type).collect();
        assert_eq!(types, ["ATTACK", "ATTACK", "SKILL", "SKILL", "POWER"]);
        assert_eq!(shop.colorless.len(), 2);
        assert_eq!(
            shop.colorless[0].card.def.rarity, "UNCOMMON",
            "无色第 1 张 UNCOMMON"
        );
        assert_eq!(
            shop.colorless[1].card.def.rarity, "RARE",
            "无色第 2 张 RARE"
        );
        assert!(shop
            .colorless
            .iter()
            .all(|c| c.card.def.color == "COLORLESS"));
        assert_eq!(shop.relics.len(), 3);
        assert_eq!(shop.potions.len(), 3);
        assert_eq!(shop.purge_cost, 75, "删卡起步价 75");
        assert!(shop.purge_available);
        assert!(shop.cards.iter().all(|c| c.card.def.color == "RED"));
        assert!(shop.sale_idx < 5);
    }

    /// 定价区间：基价 ×[0.9,1.1] 截断；无色 ×1.2；半价卡折半；
    /// 遗物/药水 = round(基价 ×[0.95,1.05])
    #[test]
    fn prices_in_range() {
        let run = force_shop(42, 0);
        let shop = run.shop.as_ref().unwrap();
        for (i, c) in shop.cards.iter().enumerate() {
            let base = card_base_price(c.card.def.rarity) as f32;
            let (lo, hi) = ((base * 0.9) as i32, (base * 1.1) as i32);
            if i == shop.sale_idx {
                // 先按原价定档再整型折半：2p ∈ [lo, hi+1]
                assert!(
                    c.price * 2 >= lo && c.price * 2 <= hi + 1,
                    "半价卡 {} 原价区间 [{lo},{hi}]，实际 {}/2",
                    c.card.def.id,
                    c.price
                );
            } else {
                assert!(
                    c.price >= lo && c.price <= hi,
                    "{} 价格 {} 越界 [{lo},{hi}]",
                    c.card.def.id,
                    c.price
                );
            }
        }
        for c in &shop.colorless {
            let base = card_base_price(c.card.def.rarity) as f32 * 1.2;
            assert!(
                c.price >= (base * 0.9) as i32 && c.price <= (base * 1.1) as i32,
                "无色 {} 价格 {}",
                c.card.def.id,
                c.price
            );
        }
        for r in &shop.relics {
            // tier 由构造序列决定，基价三档任一满足即可
            let ok = [150, 250, 300].iter().any(|&b| {
                let (lo, hi) = ((b as f32 * 0.95) as i32, (b as f32 * 1.05 + 0.5) as i32);
                r.price >= lo && r.price <= hi
            });
            assert!(ok, "遗物 {:?} 价格 {}", r.kind, r.price);
        }
        for p in &shop.potions {
            let ok = [50, 75, 100].iter().any(|&b| {
                let (lo, hi) = ((b as f32 * 0.95) as i32, (b as f32 * 1.05 + 0.5) as i32);
                p.price >= lo && p.price <= hi
            });
            assert!(ok, "药水 {:?} 价格 {}", p.kind, p.price);
        }
    }

    /// 同种子同进货（id + 价格全等）；异种子差异
    #[test]
    fn determinism() {
        let a = force_shop(123, 0);
        let b = force_shop(123, 0);
        let sa = a.shop.unwrap();
        let sb = b.shop.unwrap();
        let sig = |s: &Shop| -> Vec<(String, i32)> {
            s.cards
                .iter()
                .map(|c| (c.card.def.id.to_string(), c.price))
                .chain(
                    s.colorless
                        .iter()
                        .map(|c| (c.card.def.id.to_string(), c.price)),
                )
                .chain(s.relics.iter().map(|r| (format!("{:?}", r.kind), r.price)))
                .chain(s.potions.iter().map(|p| (format!("{:?}", p.kind), p.price)))
                .collect()
        };
        assert_eq!(sig(&sa), sig(&sb));
        assert_eq!(sa.sale_idx, sb.sale_idx);
    }

    /// 购买流转：扣款/入组/售罄；金币不足拒绝；删卡一次 + 价差 +25 跨店累计
    #[test]
    fn buy_and_purge_flow() {
        let mut run = force_shop(5, 0);
        let (price, card_id) = {
            let s = run.shop.as_ref().unwrap();
            (s.cards[0].price, s.cards[0].card.def.id)
        };
        run.player.gold = price + 10;
        assert!(run.shop_buy_card(0));
        assert_eq!(run.player.gold, 10, "扣款");
        assert!(run.deck.iter().any(|c| c.def.id == card_id), "入组");
        assert!(!run.shop_buy_card(0), "已售罄");
        assert!(!run.shop_buy_card(1), "金币不足");

        // 删卡：75 一次，购后 +25，同店不可再删
        let deck_len = run.deck.len();
        run.player.gold = 200;
        assert!(run.shop_purge(0));
        assert_eq!(run.player.gold, 200 - 75);
        assert_eq!(run.deck.len(), deck_len - 1);
        assert!(!run.shop_purge(1), "每店一次");
        assert_eq!(run.purge_cost, 100, "整局涨价");

        // 下一家店：删卡价 100
        run.map.rows[2][3].room = Some(RoomKind::Shop);
        run.pos = (2, 3);
        run.enter_room();
        assert_eq!(run.shop.as_ref().unwrap().purge_cost, 100, "跨店累计");
    }

    /// 药水购买需空槽
    #[test]
    fn potion_needs_slot() {
        let mut run = force_shop(9, 0);
        run.player.gold = 999;
        // 先灌满 3 槽
        for slot in run.potions.iter_mut() {
            *slot = Some(crate::potions::PotionInst::new(
                crate::potions::PotionKind::Fire,
            ));
        }
        assert!(!run.shop_buy_potion(0), "满槽不可买");
        run.potions[0] = None;
        assert!(run.shop_buy_potion(0), "有空槽可买");
    }

    /// A16：全场 ×1.1（round；先定价后半价再 ×1.1，半价卡单独验证）
    #[test]
    fn a16_markup() {
        let run = force_shop(77, 16);
        let shop = run.shop.as_ref().unwrap();
        for (i, c) in shop.cards.iter().enumerate() {
            if i == shop.sale_idx {
                // 半价后再 ×1.1：2p ≈ 基价 ×1.1
                let base = card_base_price(c.card.def.rarity) as f32;
                assert!(
                    (c.price as f32 * 2.0) >= base * 0.9
                        && (c.price as f32 * 2.0) <= base * 1.1 + 2.0,
                    "A16 半价卡 {} 价 {} 基准 {base}",
                    c.card.def.id,
                    c.price
                );
                continue;
            }
            let base = (card_base_price(c.card.def.rarity) as f32 * 1.1).floor();
            assert!(
                c.price as f32 >= base * 0.9 - 1.0 && c.price as f32 <= base * 1.1 + 1.0,
                "A16 卡价 {} 基准 {base}",
                c.price
            );
        }
    }
}
