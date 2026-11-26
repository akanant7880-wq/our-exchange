//! Deterministic Price-Time Priority Order Book & Matching Engine for BTCUSDT Perpetual.
//!
//! Guarantees:
//! - Strict Price-Time (FIFO) priority via `BTreeMap<PriceTicks, VecDeque<RestingOrder>>`.
//! - Zero floating-point arithmetic.
//! - Zero artificial liquidity (unfilled Market order remainders expire unfilled).
//! - Idempotent `OrderId` enforcement.
//! - Atomic Self-Trade Prevention (rejects incoming order if it crosses own resting order).
//! - Deterministic event log for exact state replay.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::types::{
    MatchOutcome, OrderId, OrderRequest, OrderType, PriceTicks, QtyLots, RejectReason,
    RestingOrder, SequenceNum, Side, Trade, TradeId,
};

/// Recorded command event used for deterministic replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    PlaceOrder(OrderRequest),
    CancelOrder { order_id: OrderId },
}

/// Top-of-book snapshot for market data inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookTop {
    pub best_bid: Option<(PriceTicks, QtyLots)>,
    pub best_ask: Option<(PriceTicks, QtyLots)>,
}

/// Single-instrument deterministic matching engine.
#[derive(Debug, Default, Clone)]
pub struct OrderBookEngine {
    /// Buy orders keyed by price ticks (highest price = best bid).
    bids: BTreeMap<PriceTicks, VecDeque<RestingOrder>>,
    /// Sell orders keyed by price ticks (lowest price = best ask).
    asks: BTreeMap<PriceTicks, VecDeque<RestingOrder>>,
    /// Fast lookup from active `OrderId` -> `(Side, PriceTicks)` for O(log P + K) cancellation.
    order_index: HashMap<OrderId, (Side, PriceTicks)>,
    /// All `OrderId`s ever submitted (enforces idempotency).
    seen_order_ids: HashSet<OrderId>,
    /// Monotonic sequence counter incremented on every accepted command.
    next_sequence: SequenceNum,
    /// Monotonic trade counter incremented on every executed fill.
    next_trade_id: TradeId,
    /// Immutable log of accepted events for deterministic replay.
    event_log: Vec<EngineEvent>,
}

impl OrderBookEngine {
    /// Creates an empty order book engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current best bid and best ask price and total resting lots at each top level.
    pub fn top_of_book(&self) -> BookTop {
        let best_bid = self.bids.iter().next_back().map(|(&price, queue)| {
            let total_lots = queue.iter().map(|o| o.remaining_lots).sum();
            (price, total_lots)
        });
        let best_ask = self.asks.iter().next().map(|(&price, queue)| {
            let total_lots = queue.iter().map(|o| o.remaining_lots).sum();
            (price, total_lots)
        });
        BookTop { best_bid, best_ask }
    }

    /// Returns a read-only reference to the recorded event log.
    pub fn events(&self) -> &[EngineEvent] {
        &self.event_log
    }

    /// Reconstructs an identical engine state by replaying a slice of recorded `EngineEvent`s.
    pub fn replay(events: &[EngineEvent]) -> Result<Self, RejectReason> {
        let mut engine = Self::new();
        for event in events {
            match event {
                EngineEvent::PlaceOrder(req) => {
                    engine.place_order(req.clone())?;
                }
                EngineEvent::CancelOrder { order_id } => {
                    engine.cancel_order(*order_id)?;
                }
            }
        }
        Ok(engine)
    }

    /// Validates and executes a Limit or Market order.
    pub fn place_order(&mut self, req: OrderRequest) -> Result<MatchOutcome, RejectReason> {
        // 1. Validate request fields
        if req.qty_lots == 0 {
            return Err(RejectReason::ZeroQuantity);
        }
        match req.order_type {
            OrderType::Limit => {
                if req.price_ticks == 0 {
                    return Err(RejectReason::InvalidLimitPrice);
                }
            }
            OrderType::Market => {
                if req.price_ticks != 0 {
                    return Err(RejectReason::MarketOrderWithPrice);
                }
            }
        }
        if self.seen_order_ids.contains(&req.order_id) {
            return Err(RejectReason::DuplicateOrderId);
        }

        // 2. Pre-check Self-Trade Prevention atomically before mutating state
        if self.would_self_trade(&req) {
            return Err(RejectReason::SelfTradePrevented);
        }

        // 3. Commit command identity and sequence number
        self.seen_order_ids.insert(req.order_id);
        self.next_sequence += 1;
        let seq = self.next_sequence;
        self.event_log.push(EngineEvent::PlaceOrder(req.clone()));

        let mut remaining_lots = req.qty_lots;
        let mut filled_lots: QtyLots = 0;
        let mut trades = Vec::new();

        // 4. Match against opposing book in strict Price-Time priority
        match req.side {
            Side::Buy => {
                while remaining_lots > 0 {
                    let best_ask_price = match self.asks.keys().next().copied() {
                        Some(p) => p,
                        None => break, // No sell liquidity left
                    };

                    if req.order_type == OrderType::Limit && best_ask_price > req.price_ticks {
                        break; // Best ask is above buyer's limit price
                    }

                    let mut level_empty = false;
                    if let Some(queue) = self.asks.get_mut(&best_ask_price) {
                        while remaining_lots > 0 {
                            let maker = match queue.front_mut() {
                                Some(m) => m,
                                None => break,
                            };

                            let fill_lots = remaining_lots.min(maker.remaining_lots);
                            remaining_lots -= fill_lots;
                            filled_lots += fill_lots;
                            maker.remaining_lots -= fill_lots;

                            self.next_trade_id += 1;
                            trades.push(Trade {
                                trade_id: self.next_trade_id,
                                sequence: seq,
                                price_ticks: best_ask_price,
                                qty_lots: fill_lots,
                                maker_order_id: maker.order_id,
                                maker_user_id: maker.user_id,
                                taker_order_id: req.order_id,
                                taker_user_id: req.user_id,
                                taker_side: Side::Buy,
                            });

                            if maker.remaining_lots == 0 {
                                let completed_id = maker.order_id;
                                queue.pop_front();
                                self.order_index.remove(&completed_id);
                            }
                        }
                        if queue.is_empty() {
                            level_empty = true;
                        }
                    }

                    if level_empty {
                        self.asks.remove(&best_ask_price);
                    }
                }
            }
            Side::Sell => {
                while remaining_lots > 0 {
                    let best_bid_price = match self.bids.keys().next_back().copied() {
                        Some(p) => p,
                        None => break, // No buy liquidity left
                    };

                    if req.order_type == OrderType::Limit && best_bid_price < req.price_ticks {
                        break; // Best bid is below seller's limit price
                    }

                    let mut level_empty = false;
                    if let Some(queue) = self.bids.get_mut(&best_bid_price) {
                        while remaining_lots > 0 {
                            let maker = match queue.front_mut() {
                                Some(m) => m,
                                None => break,
                            };

                            let fill_lots = remaining_lots.min(maker.remaining_lots);
                            remaining_lots -= fill_lots;
                            filled_lots += fill_lots;
                            maker.remaining_lots -= fill_lots;

                            self.next_trade_id += 1;
                            trades.push(Trade {
                                trade_id: self.next_trade_id,
                                sequence: seq,
                                price_ticks: best_bid_price,
                                qty_lots: fill_lots,
                                maker_order_id: maker.order_id,
                                maker_user_id: maker.user_id,
                                taker_order_id: req.order_id,
                                taker_user_id: req.user_id,
                                taker_side: Side::Sell,
                            });

                            if maker.remaining_lots == 0 {
                                let completed_id = maker.order_id;
                                queue.pop_front();
                                self.order_index.remove(&completed_id);
                            }
                        }
                        if queue.is_empty() {
                            level_empty = true;
                        }
                    }

                    if level_empty {
                        self.bids.remove(&best_bid_price);
                    }
                }
            }
        }

        // 5. Rest any unfilled Limit order remainder on the book; Market orders never rest
        let mut resting_on_book = false;
        if remaining_lots > 0 && req.order_type == OrderType::Limit {
            let resting = RestingOrder {
                order_id: req.order_id,
                user_id: req.user_id,
                side: req.side,
                price_ticks: req.price_ticks,
                remaining_lots,
                sequence: seq,
            };
            match req.side {
                Side::Buy => {
                    self.bids
                        .entry(req.price_ticks)
                        .or_default()
                        .push_back(resting);
                }
                Side::Sell => {
                    self.asks
                        .entry(req.price_ticks)
                        .or_default()
                        .push_back(resting);
                }
            }
            self.order_index
                .insert(req.order_id, (req.side, req.price_ticks));
            resting_on_book = true;
        }

        Ok(MatchOutcome {
            order_id: req.order_id,
            trades,
            filled_lots,
            remaining_lots,
            resting_on_book,
        })
    }

    /// Cancels an active resting order by `OrderId` and removes it from the book.
    pub fn cancel_order(&mut self, order_id: OrderId) -> Result<RestingOrder, RejectReason> {
        let (side, price_ticks) = self
            .order_index
            .remove(&order_id)
            .ok_or(RejectReason::OrderNotFound)?;

        self.next_sequence += 1;
        self.event_log.push(EngineEvent::CancelOrder { order_id });

        let book_side = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };

        let mut removed_order = None;
        let mut level_empty = false;

        if let Some(queue) = book_side.get_mut(&price_ticks) {
            if let Some(pos) = queue.iter().position(|o| o.order_id == order_id) {
                removed_order = queue.remove(pos);
            }
            if queue.is_empty() {
                level_empty = true;
            }
        }

        if level_empty {
            book_side.remove(&price_ticks);
        }

        removed_order.ok_or(RejectReason::OrderNotFound)
    }

    /// Checks whether an incoming order would match any resting order from the same `user_id`.
    fn would_self_trade(&self, req: &OrderRequest) -> bool {
        let mut needed = req.qty_lots;
        match req.side {
            Side::Buy => {
                for (&ask_price, queue) in &self.asks {
                    if req.order_type == OrderType::Limit && ask_price > req.price_ticks {
                        break;
                    }
                    for maker in queue {
                        if maker.user_id == req.user_id {
                            return true;
                        }
                        if maker.remaining_lots >= needed {
                            return false;
                        }
                        needed -= maker.remaining_lots;
                    }
                }
            }
            Side::Sell => {
                for (&bid_price, queue) in self.bids.iter().rev() {
                    if req.order_type == OrderType::Limit && bid_price < req.price_ticks {
                        break;
                    }
                    for maker in queue {
                        if maker.user_id == req.user_id {
                            return true;
                        }
                        if maker.remaining_lots >= needed {
                            return false;
                        }
                        needed -= maker.remaining_lots;
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UserId;

    fn limit_order(
        order_id: OrderId,
        user_id: UserId,
        side: Side,
        price_ticks: PriceTicks,
        qty_lots: QtyLots,
    ) -> OrderRequest {
        OrderRequest {
            order_id,
            user_id,
            side,
            order_type: OrderType::Limit,
            price_ticks,
            qty_lots,
        }
    }

    fn market_order(
        order_id: OrderId,
        user_id: UserId,
        side: Side,
        qty_lots: QtyLots,
    ) -> OrderRequest {
        OrderRequest {
            order_id,
            user_id,
            side,
            order_type: OrderType::Market,
            price_ticks: 0,
            qty_lots,
        }
    }

    #[test]
    fn enforces_price_time_priority_and_partial_fills() {
        let mut engine = OrderBookEngine::new();

        // Two sellers at $95,000.0 (950,000 ticks) and one better seller at $94,990.0 (949,900 ticks)
        engine
            .place_order(limit_order(1, 101, Side::Sell, 950_000, 100))
            .unwrap();
        engine
            .place_order(limit_order(2, 102, Side::Sell, 950_000, 150))
            .unwrap();
        engine
            .place_order(limit_order(3, 103, Side::Sell, 949_900, 80))
            .unwrap();

        // Incoming Buy Limit for 220 lots up to $95,000.0
        // Should fill:
        //   1) Order #3 (80 lots @ 949,900 - best price)
        //   2) Order #1 (100 lots @ 950,000 - earlier time at 950,000)
        //   3) Order #2 (40 lots @ 950,000 - partial fill, 110 lots remain)
        let outcome = engine
            .place_order(limit_order(4, 201, Side::Buy, 950_000, 220))
            .unwrap();

        assert_eq!(outcome.filled_lots, 220);
        assert_eq!(outcome.remaining_lots, 0);
        assert!(!outcome.resting_on_book);
        assert_eq!(outcome.trades.len(), 3);

        assert_eq!(outcome.trades[0].maker_order_id, 3);
        assert_eq!(outcome.trades[0].price_ticks, 949_900);
        assert_eq!(outcome.trades[0].qty_lots, 80);

        assert_eq!(outcome.trades[1].maker_order_id, 1);
        assert_eq!(outcome.trades[1].price_ticks, 950_000);
        assert_eq!(outcome.trades[1].qty_lots, 100);

        assert_eq!(outcome.trades[2].maker_order_id, 2);
        assert_eq!(outcome.trades[2].price_ticks, 950_000);
        assert_eq!(outcome.trades[2].qty_lots, 40);

        // Top of book should now show remaining 110 lots on Order #2 at 950,000
        let top = engine.top_of_book();
        assert_eq!(top.best_bid, None);
        assert_eq!(top.best_ask, Some((950_000, 110)));
    }

    #[test]
    fn market_order_never_creates_fake_liquidity() {
        let mut engine = OrderBookEngine::new();

        // Only 50 lots available on the ask side
        engine
            .place_order(limit_order(1, 101, Side::Sell, 950_000, 50))
            .unwrap();

        // Trader sends a Market Buy for 200 lots
        let outcome = engine
            .place_order(market_order(2, 202, Side::Buy, 200))
            .unwrap();

        assert_eq!(outcome.filled_lots, 50);
        assert_eq!(outcome.remaining_lots, 150);
        assert!(!outcome.resting_on_book);
        assert_eq!(engine.top_of_book().best_ask, None);
        assert_eq!(engine.top_of_book().best_bid, None);
    }

    #[test]
    fn rejects_duplicate_orders_and_self_trades() {
        let mut engine = OrderBookEngine::new();

        engine
            .place_order(limit_order(10, 500, Side::Sell, 950_000, 100))
            .unwrap();

        // Duplicate OrderId rejected
        let dup = engine.place_order(limit_order(10, 501, Side::Buy, 940_000, 50));
        assert_eq!(dup, Err(RejectReason::DuplicateOrderId));

        // Same user (500) crossing own sell order rejected atomically
        let stp = engine.place_order(limit_order(11, 500, Side::Buy, 950_000, 50));
        assert_eq!(stp, Err(RejectReason::SelfTradePrevented));

        // Original order remains untouched on the book
        assert_eq!(engine.top_of_book().best_ask, Some((950_000, 100)));
    }

    #[test]
    fn cancel_and_deterministic_replay_match_exactly() {
        let mut engine = OrderBookEngine::new();

        engine
            .place_order(limit_order(1, 101, Side::Buy, 940_000, 100))
            .unwrap();
        engine
            .place_order(limit_order(2, 102, Side::Buy, 945_000, 150))
            .unwrap();
        engine
            .place_order(limit_order(3, 103, Side::Sell, 950_000, 200))
            .unwrap();

        let canceled = engine.cancel_order(2).unwrap();
        assert_eq!(canceled.order_id, 2);
        assert_eq!(engine.top_of_book().best_bid, Some((940_000, 100)));

        // Cancel again must fail
        assert_eq!(engine.cancel_order(2), Err(RejectReason::OrderNotFound));

        // Replay recorded events on a brand-new engine
        let replayed = OrderBookEngine::replay(engine.events()).unwrap();
        assert_eq!(replayed.top_of_book(), engine.top_of_book());
        assert_eq!(replayed.events(), engine.events());
    }
}
