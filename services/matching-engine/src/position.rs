//! Integer-only Perpetual Position, P&L Tracking & Pre-Trade Risk Preview for BTCUSDT Perpetual.

use std::collections::HashMap;
use crate::ledger::MicroUsdt;
use crate::types::{notional_1e5_usdt, PriceTicks, QtyLots, Side, Trade, UserId};

pub type SignedLots = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskPreviewError {
    ZeroQuantity,
    InvalidEntryPrice,
    InvalidStopLossDirection,
    InvalidTakeProfitDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRiskPreview {
    pub side: Side,
    pub entry_price_ticks: PriceTicks,
    pub qty_lots: QtyLots,
    pub required_margin: MicroUsdt,
    pub has_sufficient_margin: bool,
    pub projected_tp_profit: Option<MicroUsdt>,
    pub projected_sl_loss: Option<MicroUsdt>,
    pub reward_to_risk_bps: Option<u64>,
    pub account_risk_bps: Option<u64>,
}

pub fn preview_order_risk(
    side: Side,
    entry_price_ticks: PriceTicks,
    qty_lots: QtyLots,
    stop_loss_ticks: Option<PriceTicks>,
    take_profit_ticks: Option<PriceTicks>,
    available_balance: MicroUsdt,
) -> Result<OrderRiskPreview, RiskPreviewError> {
    if qty_lots == 0 {
        return Err(RiskPreviewError::ZeroQuantity);
    }
    if entry_price_ticks == 0 {
        return Err(RiskPreviewError::InvalidEntryPrice);
    }

    let projected_sl_loss = match stop_loss_ticks {
        None => None,
        Some(sl) => {
            let valid = match side {
                Side::Buy => sl > 0 && sl < entry_price_ticks,
                Side::Sell => sl > entry_price_ticks,
            };
            if !valid {
                return Err(RiskPreviewError::InvalidStopLossDirection);
            }
            Some(notional_1e5_usdt(entry_price_ticks.abs_diff(sl), qty_lots) as MicroUsdt)
        }
    };

    let projected_tp_profit = match take_profit_ticks {
        None => None,
        Some(tp) => {
            let valid = match side {
                Side::Buy => tp > entry_price_ticks,
                Side::Sell => tp > 0 && tp < entry_price_ticks,
            };
            if !valid {
                return Err(RiskPreviewError::InvalidTakeProfitDirection);
            }
            Some(notional_1e5_usdt(entry_price_ticks.abs_diff(tp), qty_lots) as MicroUsdt)
        }
    };

    let required_margin = notional_1e5_usdt(entry_price_ticks, qty_lots) as MicroUsdt;
    let has_sufficient_margin = available_balance >= required_margin;

    let reward_to_risk_bps = match (projected_tp_profit, projected_sl_loss) {
        (Some(tp_profit), Some(sl_loss)) if sl_loss > 0 => {
            Some(((tp_profit as u128 * 10_000) / (sl_loss as u128)) as u64)
        }
        _ => None,
    };

    let account_risk_bps = match projected_sl_loss {
        Some(sl_loss) if available_balance > 0 => {
            Some(((sl_loss as u128 * 10_000) / (available_balance as u128)) as u64)
        }
        _ => None,
    };

    Ok(OrderRiskPreview {
        side,
        entry_price_ticks,
        qty_lots,
        required_margin,
        has_sufficient_margin,
        projected_tp_profit,
        projected_sl_loss,
        reward_to_risk_bps,
        account_risk_bps,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Position {
    pub signed_lots: SignedLots,
    pub entry_notional_1e5: u128,
    pub cumulative_realized_pnl: MicroUsdt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionFillResult {
    pub user_id: UserId,
    pub previous_signed_lots: SignedLots,
    pub new_signed_lots: SignedLots,
    pub closed_lots: QtyLots,
    pub realized_pnl: MicroUsdt,
}

impl Position {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abs_lots(&self) -> QtyLots {
        self.signed_lots.unsigned_abs()
    }

    pub fn avg_entry_price_ticks(&self) -> PriceTicks {
        let abs = self.abs_lots() as u128;
        if abs == 0 { 0 } else { (self.entry_notional_1e5 / abs) as PriceTicks }
    }

    pub fn unrealized_pnl(&self, mark_price_ticks: PriceTicks) -> MicroUsdt {
        if self.signed_lots == 0 { return 0; }
        let mark_notional = (mark_price_ticks as u128) * (self.abs_lots() as u128);
        if self.signed_lots > 0 {
            (mark_notional as i128) - (self.entry_notional_1e5 as i128)
        } else {
            (self.entry_notional_1e5 as i128) - (mark_notional as i128)
        }
    }

    pub fn apply_fill(
        &mut self,
        side: Side,
        fill_price_ticks: PriceTicks,
        fill_qty_lots: QtyLots,
    ) -> (QtyLots, MicroUsdt) {
        if fill_qty_lots == 0 { return (0, 0); }
        let fill_signed: SignedLots = match side {
            Side::Buy => fill_qty_lots as SignedLots,
            Side::Sell => -(fill_qty_lots as SignedLots),
        };
        let prev_lots = self.signed_lots;
        let prev_abs = self.abs_lots();

        if prev_lots == 0 || (prev_lots > 0 && fill_signed > 0) || (prev_lots < 0 && fill_signed < 0) {
            self.signed_lots += fill_signed;
            self.entry_notional_1e5 += (fill_price_ticks as u128) * (fill_qty_lots as u128);
            return (0, 0);
        }

        let closed_lots = prev_abs.min(fill_qty_lots);
        let remaining_open_lots = fill_qty_lots - closed_lots;
        let closed_entry_notional = (self.entry_notional_1e5 * (closed_lots as u128)) / (prev_abs as u128);
        let exit_notional = (fill_price_ticks as u128) * (closed_lots as u128);

        let realized_pnl: MicroUsdt = if prev_lots > 0 {
            (exit_notional as i128) - (closed_entry_notional as i128)
        } else {
            (closed_entry_notional as i128) - (exit_notional as i128)
        };

        self.cumulative_realized_pnl += realized_pnl;

        if remaining_open_lots == 0 {
            self.signed_lots += fill_signed;
            if self.signed_lots == 0 {
                self.entry_notional_1e5 = 0;
            } else {
                self.entry_notional_1e5 -= closed_entry_notional;
            }
        } else {
            self.signed_lots = match side {
                Side::Buy => remaining_open_lots as SignedLots,
                Side::Sell => -(remaining_open_lots as SignedLots),
            };
            self.entry_notional_1e5 = (fill_price_ticks as u128) * (remaining_open_lots as u128);
        }

        (closed_lots, realized_pnl)
    }
}

#[derive(Debug, Default, Clone)]
pub struct PositionManager {
    positions: HashMap<UserId, Position>,
}

impl PositionManager {
    pub fn new() -> Self { Self::default() }

    pub fn get(&self, user_id: UserId) -> Position {
        self.positions.get(&user_id).cloned().unwrap_or_default()
    }

    pub fn apply_trade(&mut self, trade: &Trade) -> (PositionFillResult, PositionFillResult) {
        let maker_side = match trade.taker_side { Side::Buy => Side::Sell, Side::Sell => Side::Buy };
        let maker_res = self.apply_user_fill(trade.maker_user_id, maker_side, trade.price_ticks, trade.qty_lots);
        let taker_res = self.apply_user_fill(trade.taker_user_id, trade.taker_side, trade.price_ticks, trade.qty_lots);
        (maker_res, taker_res)
    }

    pub fn apply_user_fill(&mut self, user_id: UserId, side: Side, price_ticks: PriceTicks, qty_lots: QtyLots) -> PositionFillResult {
        let pos = self.positions.entry(user_id).or_default();
        let prev = pos.signed_lots;
        let (closed_lots, realized_pnl) = pos.apply_fill(side, price_ticks, qty_lots);
        PositionFillResult { user_id, previous_signed_lots: prev, new_signed_lots: pos.signed_lots, closed_lots, realized_pnl }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::MICRO_USDT_PER_USDT;

    #[test]
    fn long_scaling_unrealized_and_partial_close_pnl() {
        let mut pos = Position::new();
        assert_eq!(pos.apply_fill(Side::Buy, 900_000, 10_000), (0, 0));
        assert_eq!(pos.apply_fill(Side::Buy, 920_000, 10_000), (0, 0));
        assert_eq!(pos.signed_lots, 20_000);
        assert_eq!(pos.avg_entry_price_ticks(), 910_000);
        assert_eq!(pos.unrealized_pnl(930_000), 4_000 * MICRO_USDT_PER_USDT);
        let (closed, realized) = pos.apply_fill(Side::Sell, 930_000, 10_000);
        assert_eq!(closed, 10_000);
        assert_eq!(realized, 2_000 * MICRO_USDT_PER_USDT);
        assert_eq!(pos.signed_lots, 10_000);
    }

    #[test]
    fn position_flip_long_to_short_realizes_pnl_and_resets_entry() {
        let mut pos = Position::new();
        pos.apply_fill(Side::Buy, 950_000, 5_000);
        let (closed, realized) = pos.apply_fill(Side::Sell, 940_000, 8_000);
        assert_eq!(closed, 5_000);
        assert_eq!(realized, -500 * MICRO_USDT_PER_USDT);
        assert_eq!(pos.signed_lots, -3_000);
        assert_eq!(pos.avg_entry_price_ticks(), 940_000);
        assert_eq!(pos.unrealized_pnl(930_000), 300 * MICRO_USDT_PER_USDT);
    }

    #[test]
    fn manager_applies_bilateral_trade_zero_sum_pnl() {
        let mut mgr = PositionManager::new();
        let t1 = Trade { trade_id: 1, sequence: 1, price_ticks: 950_000, qty_lots: 10_000, maker_order_id: 1, maker_user_id: 1, taker_order_id: 2, taker_user_id: 2, taker_side: Side::Buy };
        mgr.apply_trade(&t1);
        let t2 = Trade { trade_id: 2, sequence: 2, price_ticks: 965_000, qty_lots: 10_000, maker_order_id: 3, maker_user_id: 2, taker_order_id: 4, taker_user_id: 1, taker_side: Side::Buy };
        let (m, t) = mgr.apply_trade(&t2);
        assert_eq!(m.realized_pnl + t.realized_pnl, 0);
        assert_eq!(mgr.get(1).signed_lots, 0);
    }

    #[test]
    fn exness_style_virtual_position_risk_preview_calculates_tp_sl_and_rr() {
        let preview = preview_order_risk(Side::Buy, 950_000, 1_000, Some(940_000), Some(975_000), 20_000 * MICRO_USDT_PER_USDT).unwrap();
        assert_eq!(preview.required_margin, 9_500 * MICRO_USDT_PER_USDT);
        assert!(preview.has_sufficient_margin);
        assert_eq!(preview.projected_sl_loss, Some(100 * MICRO_USDT_PER_USDT));
        assert_eq!(preview.projected_tp_profit, Some(250 * MICRO_USDT_PER_USDT));
        assert_eq!(preview.reward_to_risk_bps, Some(25_000));
        assert_eq!(preview.account_risk_bps, Some(50));
    }

    #[test]
    fn risk_preview_rejects_invalid_tp_sl_directions() {
        let avail = 10_000 * MICRO_USDT_PER_USDT;
        assert_eq!(preview_order_risk(Side::Buy, 950_000, 100, Some(955_000), None, avail), Err(RiskPreviewError::InvalidStopLossDirection));
        assert_eq!(preview_order_risk(Side::Sell, 950_000, 100, None, Some(955_000), avail), Err(RiskPreviewError::InvalidTakeProfitDirection));
    }
}
