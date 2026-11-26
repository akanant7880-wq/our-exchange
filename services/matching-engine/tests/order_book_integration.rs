use matching_engine::engine::{BookTop, EngineEvent, OrderBookEngine};
use matching_engine::types::{
    OrderId, OrderRequest, OrderType, PriceTicks, QtyLots, RejectReason, Side, UserId,
};

fn limit(
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

fn market(
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
fn multi_trader_session_preserves_fifo_and_conservation_of_lots() {
    let mut engine = OrderBookEngine::new();

    // Seed ask side (sellers) across 3 price levels:
    // $95,100.0 (951,000), $95,200.0 (952,000), $95,300.0 (953,000)
    engine.place_order(limit(1, 10, Side::Sell, 951_000, 100)).unwrap();
    engine.place_order(limit(2, 11, Side::Sell, 951_000, 150)).unwrap();
    engine.place_order(limit(3, 12, Side::Sell, 952_000, 200)).unwrap();
    engine.place_order(limit(4, 13, Side::Sell, 953_000, 300)).unwrap();

    // Seed bid side (buyers) across 2 price levels:
    // $94,900.0 (949,000), $94,800.0 (948,000)
    engine.place_order(limit(5, 14, Side::Buy, 949_000, 250)).unwrap();
    engine.place_order(limit(6, 15, Side::Buy, 948_000, 400)).unwrap();

    assert_eq!(
        engine.top_of_book(),
        BookTop {
            best_bid: Some((949_000, 250)),
            best_ask: Some((951_000, 250)),
        }
    );

    // Cancel order #2 (150 lots at 951,000) before any match happens
    let canceled = engine.cancel_order(2).unwrap();
    assert_eq!(canceled.remaining_lots, 150);
    assert_eq!(
        engine.top_of_book().best_ask,
        Some((951_000, 100))
    );

    // Trader 20 sends a large Buy Limit up to $95,200.0 for 400 lots:
    // - Fills 100 lots from Order #1 @ 951,000
    // - Fills 200 lots from Order #3 @ 952,000
    // - Cannot touch Order #4 (@ 953,000 > limit 952,000)
    // - Remaining 100 lots rest on the bid book @ 952,000
    let buy_outcome = engine.place_order(limit(7, 20, Side::Buy, 952_000, 400)).unwrap();
    assert_eq!(buy_outcome.filled_lots, 300);
    assert_eq!(buy_outcome.remaining_lots, 100);
    assert!(buy_outcome.resting_on_book);
    assert_eq!(buy_outcome.trades.len(), 2);

    // Verify maker prices and conservation of quantity
    assert_eq!(buy_outcome.trades[0].price_ticks, 951_000);
    assert_eq!(buy_outcome.trades[0].qty_lots, 100);
    assert_eq!(buy_outcome.trades[1].price_ticks, 952_000);
    assert_eq!(buy_outcome.trades[1].qty_lots, 200);

    // New top of book: best_bid = 952,000 (100 lots), best_ask = 953,000 (300 lots)
    assert_eq!(
        engine.top_of_book(),
        BookTop {
            best_bid: Some((952_000, 100)),
            best_ask: Some((953_000, 300)),
        }
    );

    // Trader 21 sends a Market Sell for 180 lots:
    // - Fills 100 lots from Order #7 @ 952,000
    // - Fills 80 lots from Order #5 @ 949,000 (170 lots remain on Order #5)
    let sell_outcome = engine.place_order(market(8, 21, Side::Sell, 180)).unwrap();
    assert_eq!(sell_outcome.filled_lots, 180);
    assert_eq!(sell_outcome.remaining_lots, 0);
    assert!(!sell_outcome.resting_on_book);
    assert_eq!(sell_outcome.trades.len(), 2);
    assert_eq!(sell_outcome.trades[0].price_ticks, 952_000);
    assert_eq!(sell_outcome.trades[1].price_ticks, 949_000);

    assert_eq!(
        engine.top_of_book(),
        BookTop {
            best_bid: Some((949_000, 170)),
            best_ask: Some((953_000, 300)),
        }
    );
}

#[test]
fn validates_malformed_requests_and_historical_duplicate_ids() {
    let mut engine = OrderBookEngine::new();

    assert_eq!(
        engine.place_order(limit(1, 10, Side::Buy, 950_000, 0)),
        Err(RejectReason::ZeroQuantity)
    );
    assert_eq!(
        engine.place_order(limit(2, 10, Side::Buy, 0, 100)),
        Err(RejectReason::InvalidLimitPrice)
    );
    assert_eq!(
        engine.place_order(market(3, 10, Side::Buy, 0)),
        Err(RejectReason::ZeroQuantity)
    );
    assert_eq!(
        engine.place_order(OrderRequest {
            order_id: 4,
            user_id: 10,
            side: Side::Buy,
            order_type: OrderType::Market,
            price_ticks: 950_000,
            qty_lots: 100,
        }),
        Err(RejectReason::MarketOrderWithPrice)
    );

    // Place valid order #10, cancel it, then ensure order #10 cannot be reused
    engine.place_order(limit(10, 10, Side::Buy, 950_000, 50)).unwrap();
    engine.cancel_order(10).unwrap();
    assert_eq!(
        engine.place_order(limit(10, 10, Side::Buy, 950_000, 50)),
        Err(RejectReason::DuplicateOrderId)
    );
}

#[test]
fn self_trade_prevention_is_atomic_even_behind_other_orders() {
    let mut engine = OrderBookEngine::new();

    // User 10 rests 50 lots @ 950,000, followed by User 99 resting 50 lots @ 950,000
    engine.place_order(limit(1, 10, Side::Sell, 950_000, 50)).unwrap();
    engine.place_order(limit(2, 99, Side::Sell, 950_000, 50)).unwrap();

    // User 99 tries to buy 80 lots @ 950,000 (would fill User 10's 50 lots and hit User 99's own 30 lots)
    let res = engine.place_order(limit(3, 99, Side::Buy, 950_000, 80));
    assert_eq!(res, Err(RejectReason::SelfTradePrevented));

    // Confirm atomic rejection: User 10's 50 lots were NOT partially filled
    assert_eq!(
        engine.top_of_book().best_ask,
        Some((950_000, 100))
    );
}

#[test]
fn full_event_log_replays_deterministically() {
    let mut primary = OrderBookEngine::new();

    let commands = vec![
        EngineEvent::PlaceOrder(limit(1, 1, Side::Sell, 960_000, 100)),
        EngineEvent::PlaceOrder(limit(2, 2, Side::Sell, 955_000, 120)),
        EngineEvent::PlaceOrder(limit(3, 3, Side::Buy, 950_000, 200)),
        EngineEvent::PlaceOrder(limit(4, 4, Side::Buy, 955_000, 50)),
        EngineEvent::CancelOrder { order_id: 1 },
        EngineEvent::PlaceOrder(market(5, 5, Side::Sell, 100)),
    ];

    for cmd in &commands {
        match cmd {
            EngineEvent::PlaceOrder(req) => {
                primary.place_order(req.clone()).unwrap();
            }
            EngineEvent::CancelOrder { order_id } => {
                primary.cancel_order(*order_id).unwrap();
            }
        }
    }

    let replayed = OrderBookEngine::replay(primary.events()).unwrap();
    assert_eq!(replayed.top_of_book(), primary.top_of_book());
    assert_eq!(replayed.events(), primary.events());
}
