//! Unified Margin, Risk & Automated TP/SL Execution Engine for BTCUSDT Perpetual.

use std::collections::HashMap;
use crate::engine::{BookTop, OrderBookEngine};
use crate::ledger::{AccountKey, DoubleEntryLedger, EntrySide, LedgerError, LedgerLeg, MicroUsdt};
use crate::position::{Position, PositionManager};
use crate::types::{
    notional_1e5_usdt, OrderId, OrderRequest, OrderType, PriceTicks, QtyLots, RejectReason, Side,
    Trade, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedOrderRequest {
    pub order_id: OrderId,
    pub user_id: UserId,
    pub side: Side,
    pub order_type: OrderType,
    pub price_ticks: PriceTicks,
    pub qty_lots: QtyLots,
    pub stop_loss_ticks: Option<PriceTicks>,
    pub take_profit_ticks: Option<PriceTicks>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TpSlRule {
    pub stop_loss_ticks: Option<PriceTicks>,
    pub take_profit_ticks: Option<PriceTicks>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskEngineError {
    MatchingRejected(RejectReason),
    LedgerRejected(LedgerError),
    NoLiquidityForMarketOrder,
    InvalidTpSlLevel,
    OrderMetadataNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub order_id: OrderId,
    pub trades: Vec<Trade>,
    pub filled_lots: QtyLots,
    pub remaining_lots: QtyLots,
    pub resting_on_book: bool,
    pub triggered_orders: Vec<OrderId>,
}

#[derive(Debug, Default, Clone)]
pub struct ExchangeEngine {
    pub book: OrderBookEngine,
    pub ledger: DoubleEntryLedger,
    pub positions: PositionManager,
    /// Tracks locked margin per active resting order (`OrderId -> (UserId, MicroUsdt)`).
    order_locked_margin: HashMap<OrderId, (UserId, MicroUsdt)>,
    /// Tracks locked margin per open position (`UserId -> MicroUsdt`).
    position_locked_margin: HashMap<UserId, MicroUsdt>,
    /// Active TP/SL rules attached to a user's open position.
    active_tpsl: HashMap<UserId, TpSlRule>,
    /// Internal monotonic order ID counter for automated TP/SL trigger orders.
    next_system_order_id: OrderId,
}

impl ExchangeEngine {
    pub fn new() -> Self {
        Self {
            next_system_order_id: 9_000_000_000,
            ..Self::default()
        }
    }

    pub fn top_of_book(&self) -> BookTop {
        self.book.top_of_book()
    }

    pub fn available_balance(&self, user_id: UserId) -> MicroUsdt {
        self.ledger.balance_of(AccountKey::UserAvailable(user_id))
    }

    pub fn reserved_margin(&self, user_id: UserId) -> MicroUsdt {
        self.ledger.balance_of(AccountKey::UserReservedMargin(user_id))
    }

    pub fn position_of(&self, user_id: UserId) -> Position {
        self.positions.get(user_id)
    }

    pub fn active_tpsl_of(&self, user_id: UserId) -> Option<TpSlRule> {
        self.active_tpsl.get(&user_id).copied()
    }

    pub fn deposit_simulated_usdt(
        &mut self,
        idempotency_key: impl Into<String>,
        user_id: UserId,
        amount: MicroUsdt,
    ) -> Result<(), RiskEngineError> {
        self.ledger
            .grant_simulated_funds(idempotency_key, user_id, amount)
            .map(|_| ())
            .map_err(RiskEngineError::LedgerRejected)
    }
}

impl ExchangeEngine {
    pub fn place_managed_order(
        &mut self,
        req: ManagedOrderRequest,
    ) -> Result<ExecutionReport, RiskEngineError> {
        if req.qty_lots == 0 {
            return Err(RiskEngineError::MatchingRejected(RejectReason::ZeroQuantity));
        }

        // Validate TP/SL directions against order price when provided
        let ref_price = match req.order_type {
            OrderType::Limit => {
                if req.price_ticks == 0 {
                    return Err(RiskEngineError::MatchingRejected(RejectReason::InvalidLimitPrice));
                }
                req.price_ticks
            }
            OrderType::Market => {
                if req.price_ticks != 0 {
                    return Err(RiskEngineError::MatchingRejected(RejectReason::MarketOrderWithPrice));
                }
                match req.side {
                    Side::Buy => self.book.top_of_book().best_ask.map(|(p, _)| p)
                        .ok_or(RiskEngineError::NoLiquidityForMarketOrder)?,
                    Side::Sell => self.book.top_of_book().best_bid.map(|(p, _)| p)
                        .ok_or(RiskEngineError::NoLiquidityForMarketOrder)?,
                }
            }
        };

        if let Some(sl) = req.stop_loss_ticks {
            let valid = match req.side {
                Side::Buy => sl > 0 && sl < ref_price,
                Side::Sell => sl > ref_price,
            };
            if !valid { return Err(RiskEngineError::InvalidTpSlLevel); }
        }
        if let Some(tp) = req.take_profit_ticks {
            let valid = match req.side {
                Side::Buy => tp > ref_price,
                Side::Sell => tp > 0 && tp < ref_price,
            };
            if !valid { return Err(RiskEngineError::InvalidTpSlLevel); }
        }

        // Determine how many lots are opening/increasing vs closing an existing opposing position
        let current_pos = self.positions.get(req.user_id);
        let closing_lots = match (req.side, current_pos.signed_lots) {
            (Side::Buy, s) if s < 0 => s.unsigned_abs().min(req.qty_lots),
            (Side::Sell, s) if s > 0 => s.unsigned_abs().min(req.qty_lots),
            _ => 0,
        };
        let opening_lots = req.qty_lots - closing_lots;

        // Calculate worst-case margin needed for any opening/increasing portion
        let worst_price = match req.order_type {
            OrderType::Limit => req.price_ticks,
            OrderType::Market => self.worst_fill_price_for_market(req.side, req.qty_lots)
                .ok_or(RiskEngineError::NoLiquidityForMarketOrder)?,
        };
        let margin_to_lock = notional_1e5_usdt(worst_price, opening_lots) as MicroUsdt;

        if margin_to_lock > 0 {
            self.ledger
                .reserve_margin(format!("ord-lock-{}", req.order_id), req.user_id, margin_to_lock)
                .map_err(RiskEngineError::LedgerRejected)?;
        }

        // Execute on the deterministic matching engine
        let raw_req = OrderRequest {
            order_id: req.order_id,
            user_id: req.user_id,
            side: req.side,
            order_type: req.order_type,
            price_ticks: req.price_ticks,
            qty_lots: req.qty_lots,
        };

        let outcome = match self.book.place_order(raw_req) {
            Ok(o) => o,
            Err(e) => {
                if margin_to_lock > 0 {
                    let _ = self.ledger.release_margin(
                        format!("ord-rollback-{}", req.order_id),
                        req.user_id,
                        margin_to_lock,
                    );
                }
                return Err(RiskEngineError::MatchingRejected(e));
            }
        };

        // Settle every executed trade atomically in PositionManager & DoubleEntryLedger
        let mut taker_locked_remaining = margin_to_lock;
        let mut last_trade_price: Option<PriceTicks> = None;

        for trade in &outcome.trades {
            last_trade_price = Some(trade.price_ticks);
            let actual_notional = notional_1e5_usdt(trade.price_ticks, trade.qty_lots) as MicroUsdt;
            let (maker_res, taker_res) = self.positions.apply_trade(trade);

            // Settle Maker side
            self.settle_participant_fill(
                trade.trade_id,
                "maker",
                &maker_res,
                trade.qty_lots,
                actual_notional,
                Some(trade.maker_order_id),
                None,
            )?;

            // Settle Taker side (using taker's locked margin pool)
            self.settle_participant_fill(
                trade.trade_id,
                "taker",
                &taker_res,
                trade.qty_lots,
                actual_notional,
                None,
                Some(&mut taker_locked_remaining),
            )?;
        }

        // Handle any remaining locked margin on the taker order
        if outcome.resting_on_book {
            if taker_locked_remaining > 0 {
                self.order_locked_margin
                    .insert(req.order_id, (req.user_id, taker_locked_remaining));
            }
        } else if taker_locked_remaining > 0 {
            self.ledger
                .release_margin(
                    format!("ord-excess-refund-{}", req.order_id),
                    req.user_id,
                    taker_locked_remaining,
                )
                .map_err(RiskEngineError::LedgerRejected)?;
        }

        // Attach TP/SL rule if requested and user holds an open position
        if (req.stop_loss_ticks.is_some() || req.take_profit_ticks.is_some())
            && self.positions.get(req.user_id).signed_lots != 0
        {
            self.active_tpsl.insert(
                req.user_id,
                TpSlRule {
                    stop_loss_ticks: req.stop_loss_ticks,
                    take_profit_ticks: req.take_profit_ticks,
                },
            );
        }

        // Check if last trade price triggered any user's Stop-Loss or Take-Profit
        let mut triggered_orders = Vec::new();
        if let Some(mark_price) = last_trade_price {
            triggered_orders = self.evaluate_and_execute_tpsl(mark_price)?;
        }

        Ok(ExecutionReport {
            order_id: outcome.order_id,
            trades: outcome.trades,
            filled_lots: outcome.filled_lots,
            remaining_lots: outcome.remaining_lots,
            resting_on_book: outcome.resting_on_book,
            triggered_orders,
        })
    }
}

impl ExchangeEngine {
    pub fn cancel_managed_order(&mut self, order_id: OrderId) -> Result<(), RiskEngineError> {
        let canceled = self
            .book
            .cancel_order(order_id)
            .map_err(RiskEngineError::MatchingRejected)?;

        if let Some((user_id, locked_amt)) = self.order_locked_margin.remove(&order_id) {
            if locked_amt > 0 {
                self.ledger
                    .release_margin(
                        format!("ord-cancel-release-{}", canceled.order_id),
                        user_id,
                        locked_amt,
                    )
                    .map_err(RiskEngineError::LedgerRejected)?;
            }
        }
        Ok(())
    }

    fn worst_fill_price_for_market(&self, side: Side, qty_lots: QtyLots) -> Option<PriceTicks> {
        if qty_lots == 0 {
            return None;
        }
        match side {
            Side::Buy => self.book.top_of_book().best_ask.map(|(p, _)| p * 2),
            Side::Sell => self.book.top_of_book().best_bid.map(|(p, _)| p),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn settle_participant_fill(
        &mut self,
        trade_id: u64,
        role: &str,
        fill_res: &crate::position::PositionFillResult,
        fill_qty_lots: QtyLots,
        actual_notional: MicroUsdt,
        maker_order_id: Option<OrderId>,
        mut taker_locked_pool: Option<&mut MicroUsdt>,
    ) -> Result<(), RiskEngineError> {
        let user_id = fill_res.user_id;
        let prev_abs = fill_res.previous_signed_lots.unsigned_abs();
        let closed_lots = fill_res.closed_lots;
        let opened_lots = fill_qty_lots - closed_lots;

        // 1. Settle closed portion: release proportional position margin + settle realized P&L
        if closed_lots > 0 && prev_abs > 0 {
            let pos_locked = *self.position_locked_margin.get(&user_id).unwrap_or(&0);
            let released_pos_margin =
                ((pos_locked as u128 * closed_lots as u128) / prev_abs as u128) as MicroUsdt;

            if released_pos_margin > 0 {
                *self.position_locked_margin.entry(user_id).or_insert(0) -= released_pos_margin;
                self.ledger
                    .release_margin(
                        format!("trd-{}-{}-close-margin", trade_id, role),
                        user_id,
                        released_pos_margin,
                    )
                    .map_err(RiskEngineError::LedgerRejected)?;
            }

            // Settle realized P&L against SystemTreasury clearing pool (bilateral zero-sum across trades)
            if fill_res.realized_pnl > 0 {
                self.ledger
                    .post_entry(
                        format!("trd-{}-{}-pnl-win", trade_id, role),
                        format!("Realized profit on trade {}", trade_id),
                        vec![
                            LedgerLeg {
                                account: AccountKey::SystemTreasury,
                                side: EntrySide::Debit,
                                amount: fill_res.realized_pnl,
                            },
                            LedgerLeg {
                                account: AccountKey::UserAvailable(user_id),
                                side: EntrySide::Credit,
                                amount: fill_res.realized_pnl,
                            },
                        ],
                    )
                    .map_err(RiskEngineError::LedgerRejected)?;
            } else if fill_res.realized_pnl < 0 {
                let loss = -fill_res.realized_pnl;
                let avail = self.available_balance(user_id);
                let payable_loss = loss.min(avail);
                if payable_loss > 0 {
                    self.ledger
                        .post_entry(
                            format!("trd-{}-{}-pnl-loss", trade_id, role),
                            format!("Realized loss on trade {}", trade_id),
                            vec![
                                LedgerLeg {
                                    account: AccountKey::UserAvailable(user_id),
                                    side: EntrySide::Debit,
                                    amount: payable_loss,
                                },
                                LedgerLeg {
                                    account: AccountKey::SystemTreasury,
                                    side: EntrySide::Credit,
                                    amount: payable_loss,
                                },
                            ],
                        )
                        .map_err(RiskEngineError::LedgerRejected)?;
                }
            }

            if fill_res.new_signed_lots == 0 {
                self.active_tpsl.remove(&user_id);
            }
        }

        // 2. Settle opened portion: move margin from order pool into position locked margin
        if opened_lots > 0 {
            let opened_notional =
                ((actual_notional as u128 * opened_lots as u128) / fill_qty_lots as u128) as MicroUsdt;

            if let Some(oid) = maker_order_id {
                if let Some((_, ord_locked)) = self.order_locked_margin.get_mut(&oid) {
                    let consume = (*ord_locked).min(opened_notional);
                    *ord_locked -= consume;
                    *self.position_locked_margin.entry(user_id).or_insert(0) += consume;
                    if *ord_locked == 0 {
                        self.order_locked_margin.remove(&oid);
                    }
                }
            } else if let Some(pool) = taker_locked_pool.as_deref_mut() {
                let consume = (*pool).min(opened_notional);
                *pool -= consume;
                *self.position_locked_margin.entry(user_id).or_insert(0) += consume;
            }
        }

        Ok(())
    }

    fn evaluate_and_execute_tpsl(
        &mut self,
        mark_price: PriceTicks,
    ) -> Result<Vec<OrderId>, RiskEngineError> {
        let mut to_close: Vec<(UserId, Side, QtyLots)> = Vec::new();

        for (&user_id, rule) in &self.active_tpsl {
            let pos = self.positions.get(user_id);
            if pos.signed_lots == 0 {
                continue;
            }
            let is_long = pos.signed_lots > 0;
            let hit_sl = match rule.stop_loss_ticks {
                Some(sl) if is_long => mark_price <= sl,
                Some(sl) => mark_price >= sl,
                None => false,
            };
            let hit_tp = match rule.take_profit_ticks {
                Some(tp) if is_long => mark_price >= tp,
                Some(tp) => mark_price <= tp,
                None => false,
            };
            if hit_sl || hit_tp {
                let close_side = if is_long { Side::Sell } else { Side::Buy };
                to_close.push((user_id, close_side, pos.abs_lots()));
            }
        }

        let mut triggered_ids = Vec::new();
        for (user_id, close_side, qty) in to_close {
            self.active_tpsl.remove(&user_id);
            self.next_system_order_id += 1;
            let sys_oid = self.next_system_order_id;
            let close_req = ManagedOrderRequest {
                order_id: sys_oid,
                user_id,
                side: close_side,
                order_type: OrderType::Market,
                price_ticks: 0,
                qty_lots: qty,
                stop_loss_ticks: None,
                take_profit_ticks: None,
            };
            if self.place_managed_order(close_req).is_ok() {
                triggered_ids.push(sys_oid);
            }
        }

        Ok(triggered_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::MICRO_USDT_PER_USDT;

    #[test]
    fn blocks_order_without_sufficient_margin_and_refunds_on_cancel() {
        let mut ex = ExchangeEngine::new();
        ex.deposit_simulated_usdt("dep-1", 1, 5_000 * MICRO_USDT_PER_USDT).unwrap();

        // Trying to buy 0.1 BTC (1,000 lots) @ $95,000.0 requires $9,500 margin -> must fail
        let too_big = ManagedOrderRequest {
            order_id: 1,
            user_id: 1,
            side: Side::Buy,
            order_type: OrderType::Limit,
            price_ticks: 950_000,
            qty_lots: 1_000,
            stop_loss_ticks: None,
            take_profit_ticks: None,
        };
        assert!(matches!(
            ex.place_managed_order(too_big),
            Err(RiskEngineError::LedgerRejected(LedgerError::InsufficientFunds { .. }))
        ));
        assert_eq!(ex.top_of_book().best_bid, None);

        // Valid buy for 0.04 BTC (400 lots) @ $95,000.0 locks $3,800 margin
        let valid = ManagedOrderRequest {
            order_id: 2,
            user_id: 1,
            side: Side::Buy,
            order_type: OrderType::Limit,
            price_ticks: 950_000,
            qty_lots: 400,
            stop_loss_ticks: None,
            take_profit_ticks: None,
        };
        ex.place_managed_order(valid).unwrap();
        assert_eq!(ex.available_balance(1), 1_200 * MICRO_USDT_PER_USDT);
        assert_eq!(ex.reserved_margin(1), 3_800 * MICRO_USDT_PER_USDT);

        // Canceling unlocks full $3,800 back to available balance
        ex.cancel_managed_order(2).unwrap();
        assert_eq!(ex.available_balance(1), 5_000 * MICRO_USDT_PER_USDT);
        assert_eq!(ex.reserved_margin(1), 0);
        assert!(ex.ledger.verify_global_zero_sum());
    }

    #[test]
    fn automated_take_profit_executes_when_market_reaches_target() {
        let mut ex = ExchangeEngine::new();
        ex.deposit_simulated_usdt("dep-maker", 1, 50_000 * MICRO_USDT_PER_USDT).unwrap();
        ex.deposit_simulated_usdt("dep-taker", 2, 50_000 * MICRO_USDT_PER_USDT).unwrap();
        ex.deposit_simulated_usdt("dep-mm", 3, 50_000 * MICRO_USDT_PER_USDT).unwrap();

        // Maker (User 1) rests Sell 0.1 BTC (1,000 lots) @ $95,000.0
        ex.place_managed_order(ManagedOrderRequest {
            order_id: 1,
            user_id: 1,
            side: Side::Sell,
            order_type: OrderType::Limit,
            price_ticks: 950_000,
            qty_lots: 1_000,
            stop_loss_ticks: None,
            take_profit_ticks: None,
        }).unwrap();

        // Taker (User 2) buys 0.1 BTC @ $95,000.0 with SL @ $94,000.0 and TP @ $96,000.0
        ex.place_managed_order(ManagedOrderRequest {
            order_id: 2,
            user_id: 2,
            side: Side::Buy,
            order_type: OrderType::Limit,
            price_ticks: 950_000,
            qty_lots: 1_000,
            stop_loss_ticks: Some(940_000),
            take_profit_ticks: Some(960_000),
        }).unwrap();
        assert_eq!(ex.position_of(2).signed_lots, 1_000);

        // Market Maker (User 3) places a Buy Bid @ $96,000.0 (960,000) and trades at $96,000.0
        ex.place_managed_order(ManagedOrderRequest {
            order_id: 3,
            user_id: 3,
            side: Side::Buy,
            order_type: OrderType::Limit,
            price_ticks: 960_000,
            qty_lots: 2_000,
            stop_loss_ticks: None,
            take_profit_ticks: None,
        }).unwrap();

        // Trigger a trade at $96,000.0 -> User 2's TP ($96,000.0) fires automatically and closes Long!
        let report = ex.place_managed_order(ManagedOrderRequest {
            order_id: 4,
            user_id: 1,
            side: Side::Sell,
            order_type: OrderType::Market,
            price_ticks: 0,
            qty_lots: 500,
            stop_loss_ticks: None,
            take_profit_ticks: None,
        }).unwrap();

        assert_eq!(report.triggered_orders.len(), 1);
        assert_eq!(ex.position_of(2).signed_lots, 0);
        // User 2 gained +$100 profit ($95k -> $96k on 0.1 BTC) -> $50,100 total available
        assert_eq!(ex.available_balance(2), 50_100 * MICRO_USDT_PER_USDT);
        assert!(ex.ledger.verify_global_zero_sum());
    }
}
