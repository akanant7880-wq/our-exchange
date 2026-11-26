//! Core fixed-point domain types for the BTCUSDT Perpetual matching engine.
//!
//! Precision rules (NO floating-point numbers allowed):
//! - Price: 1 tick = 0.1 USDT (10^-1 USDT). Example: 95,000.5 USDT = 950_005 ticks.
//! - Quantity: 1 lot = 0.0001 BTC (10^-4 BTC). Example: 0.2500 BTC = 2_500 lots.

/// Price measured in integer ticks (1 tick = 0.1 USDT).
pub type PriceTicks = u64;

/// Contract quantity measured in integer lots (1 lot = 0.0001 BTC).
pub type QtyLots = u64;

/// Unique monotonic order identifier.
pub type OrderId = u64;

/// Unique user/account identifier.
pub type UserId = u64;

/// Unique monotonic trade identifier.
pub type TradeId = u64;

/// Monotonic sequence number for deterministic event replay.
pub type SequenceNum = u64;

/// Contract specification constants for BTCUSDT Perpetual.
pub const SYMBOL: &str = "BTCUSDT-PERP";
pub const TICK_SCALE: u64 = 10;      // 1 USDT = 10 price ticks (0.1 USDT per tick)
pub const LOT_SCALE: u64 = 10_000;   // 1 BTC = 10,000 lots (0.0001 BTC per lot)

/// Order direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Supported order types in Phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Limit,
    Market,
}

/// Incoming order request submitted to the matching engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRequest {
    pub order_id: OrderId,
    pub user_id: UserId,
    pub side: Side,
    pub order_type: OrderType,
    /// Required for Limit orders (must be > 0). Ignored (must be 0) for Market orders.
    pub price_ticks: PriceTicks,
    /// Quantity in integer lots (must be > 0).
    pub qty_lots: QtyLots,
}

/// Active resting order stored inside the order book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestingOrder {
    pub order_id: OrderId,
    pub user_id: UserId,
    pub side: Side,
    pub price_ticks: PriceTicks,
    pub remaining_lots: QtyLots,
    pub sequence: SequenceNum,
}

/// Executed trade between a resting maker order and an incoming taker order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trade {
    pub trade_id: TradeId,
    pub sequence: SequenceNum,
    pub price_ticks: PriceTicks,
    pub qty_lots: QtyLots,
    pub maker_order_id: OrderId,
    pub maker_user_id: UserId,
    pub taker_order_id: OrderId,
    pub taker_user_id: UserId,
    pub taker_side: Side,
}

/// Reasons an incoming order or cancel request can be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    ZeroQuantity,
    InvalidLimitPrice,
    MarketOrderWithPrice,
    DuplicateOrderId,
    SelfTradePrevented,
    OrderNotFound,
}

/// Result of processing a single order through the matching engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOutcome {
    pub order_id: OrderId,
    pub trades: Vec<Trade>,
    pub filled_lots: QtyLots,
    pub remaining_lots: QtyLots,
    /// True if remaining quantity was placed on the book (Limit orders only).
    pub resting_on_book: bool,
}

/// Helper to compute notional value in micro-USDT (10^-5 USDT) without overflow.
/// Example: 950_000 ticks ($95,000.0) * 10_000 lots (1.0 BTC) = 9_500_000_000 (10^-5 USDT) = $95,000.00000.
pub fn notional_1e5_usdt(price_ticks: PriceTicks, qty_lots: QtyLots) -> u128 {
    (price_ticks as u128) * (qty_lots as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notional_calculation_is_exact_without_floats() {
        // 1 BTC (10,000 lots) at $95,000.50 (950,005 ticks)
        // Expected: 95,000.5 * 10^5 = 9,500,050,000
        let notional = notional_1e5_usdt(950_005, 10_000);
        assert_eq!(notional, 9_500_050_000);
    }
}
