//! Line-delimited JSON IPC worker wrapping `ExchangeEngine` for the Node.js API server.
//! Uses zero external crates to keep binary size and Cloud Shell disk usage minimal.

use std::io::{self, BufRead, Write};

use matching_engine::ledger::MICRO_USDT_PER_USDT;
use matching_engine::position::preview_order_risk;
use matching_engine::risk_engine::{ExchangeEngine, ManagedOrderRequest};
use matching_engine::types::{OrderId, OrderType, PriceTicks, QtyLots, Side, UserId};

fn extract_str<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\":\"", key);
    let start = line.find(&pattern)? + pattern.len();
    let end = line[start..].find('"')? + start;
    Some(&line[start..end])
}

fn extract_u64(line: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{}\":", key);
    let start = line.find(&pattern)? + pattern.len();
    let rest = line[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

fn extract_i128(line: &str, key: &str) -> Option<i128> {
    let pattern = format!("\"{}\":", key);
    let start = line.find(&pattern)? + pattern.len();
    let rest = line[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse::<i128>().ok()
}

fn parse_side(s: &str) -> Option<Side> {
    match s {
        "BUY" | "Buy" => Some(Side::Buy),
        "SELL" | "Sell" => Some(Side::Sell),
        _ => None,
    }
}

fn parse_order_type(s: &str) -> Option<OrderType> {
    match s {
        "LIMIT" | "Limit" => Some(OrderType::Limit),
        "MARKET" | "Market" => Some(OrderType::Market),
        _ => None,
    }
}

fn handle_command(engine: &mut ExchangeEngine, line: &str) -> String {
    let cmd = extract_str(line, "cmd").unwrap_or("");
    match cmd {
        "DEPOSIT" => {
            let user_id = extract_u64(line, "user_id").unwrap_or(0) as UserId;
            let amount = extract_i128(line, "amount").unwrap_or(0);
            let key = extract_str(line, "key").unwrap_or("deposit-default");
            match engine.deposit_simulated_usdt(key, user_id, amount) {
                Ok(()) => format!(
                    "{{\"ok\":true,\"user_id\":{},\"available\":{}}}",
                    user_id,
                    engine.available_balance(user_id)
                ),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{:?}\"}}", e),
            }
        }
        "PREVIEW_RISK" => {
            let side = match extract_str(line, "side").and_then(parse_side) {
                Some(s) => s,
                None => return "{\"ok\":false,\"error\":\"InvalidSide\"}".to_string(),
            };
            let entry = extract_u64(line, "price_ticks").unwrap_or(0) as PriceTicks;
            let qty = extract_u64(line, "qty_lots").unwrap_or(0) as QtyLots;
            let sl = extract_u64(line, "stop_loss_ticks").filter(|&v| v > 0);
            let tp = extract_u64(line, "take_profit_ticks").filter(|&v| v > 0);
            let user_id = extract_u64(line, "user_id").unwrap_or(0) as UserId;
            let avail = engine.available_balance(user_id);

            match preview_order_risk(side, entry, qty, sl, tp, avail) {
                Ok(p) => format!(
                    "{{\"ok\":true,\"required_margin\":{},\"has_sufficient_margin\":{},\"projected_tp_profit\":{},\"projected_sl_loss\":{},\"reward_to_risk_bps\":{},\"account_risk_bps\":{}}}",
                    p.required_margin,
                    p.has_sufficient_margin,
                    p.projected_tp_profit.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    p.projected_sl_loss.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    p.reward_to_risk_bps.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    p.account_risk_bps.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                ),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{:?}\"}}", e),
            }
        }
        "PLACE_ORDER" => {
            let order_id = extract_u64(line, "order_id").unwrap_or(0) as OrderId;
            let user_id = extract_u64(line, "user_id").unwrap_or(0) as UserId;
            let side = match extract_str(line, "side").and_then(parse_side) {
                Some(s) => s,
                None => return "{\"ok\":false,\"error\":\"InvalidSide\"}".to_string(),
            };
            let order_type = match extract_str(line, "order_type").and_then(parse_order_type) {
                Some(t) => t,
                None => return "{\"ok\":false,\"error\":\"InvalidOrderType\"}".to_string(),
            };
            let price_ticks = extract_u64(line, "price_ticks").unwrap_or(0) as PriceTicks;
            let qty_lots = extract_u64(line, "qty_lots").unwrap_or(0) as QtyLots;
            let stop_loss_ticks = extract_u64(line, "stop_loss_ticks").filter(|&v| v > 0);
            let take_profit_ticks = extract_u64(line, "take_profit_ticks").filter(|&v| v > 0);

            let req = ManagedOrderRequest {
                order_id,
                user_id,
                side,
                order_type,
                price_ticks,
                qty_lots,
                stop_loss_ticks,
                take_profit_ticks,
            };

            match engine.place_managed_order(req) {
                Ok(rep) => {
                    let trades_json: Vec<String> = rep
                        .trades
                        .iter()
                        .map(|t| {
                            format!(
                                "{{\"trade_id\":{},\"price_ticks\":{},\"qty_lots\":{},\"maker_user_id\":{},\"taker_user_id\":{}}}",
                                t.trade_id, t.price_ticks, t.qty_lots, t.maker_user_id, t.taker_user_id
                            )
                        })
                        .collect();
                    format!(
                        "{{\"ok\":true,\"order_id\":{},\"filled_lots\":{},\"remaining_lots\":{},\"resting_on_book\":{},\"triggered_count\":{},\"trades\":[{}]}}",
                        rep.order_id,
                        rep.filled_lots,
                        rep.remaining_lots,
                        rep.resting_on_book,
                        rep.triggered_orders.len(),
                        trades_json.join(",")
                    )
                }
                Err(e) => format!("{{\"ok\":false,\"error\":\"{:?}\"}}", e),
            }
        }
        "CANCEL_ORDER" => {
            let order_id = extract_u64(line, "order_id").unwrap_or(0) as OrderId;
            match engine.cancel_managed_order(order_id) {
                Ok(()) => format!("{{\"ok\":true,\"canceled_order_id\":{}}}", order_id),
                Err(e) => format!("{{\"ok\":false,\"error\":\"{:?}\"}}", e),
            }
        }
        "ACCOUNT_STATE" => {
            let user_id = extract_u64(line, "user_id").unwrap_or(0) as UserId;
            let pos = engine.position_of(user_id);
            let top = engine.top_of_book();
            let mark = top
                .best_bid
                .map(|(p, _)| p)
                .or_else(|| top.best_ask.map(|(p, _)| p))
                .unwrap_or(0);
            format!(
                "{{\"ok\":true,\"user_id\":{},\"available\":{},\"reserved\":{},\"signed_lots\":{},\"avg_entry_ticks\":{},\"unrealized_pnl\":{},\"realized_pnl\":{},\"scale\":{}}}",
                user_id,
                engine.available_balance(user_id),
                engine.reserved_margin(user_id),
                pos.signed_lots,
                pos.avg_entry_price_ticks(),
                pos.unrealized_pnl(mark),
                pos.cumulative_realized_pnl,
                MICRO_USDT_PER_USDT
            )
        }
        "BOOK_TOP" => {
            let top = engine.top_of_book();
            let bid = top
                .best_bid
                .map(|(p, q)| format!("[{},{}]", p, q))
                .unwrap_or_else(|| "null".into());
            let ask = top
                .best_ask
                .map(|(p, q)| format!("[{},{}]", p, q))
                .unwrap_or_else(|| "null".into());
            format!("{{\"ok\":true,\"best_bid\":{},\"best_ask\":{}}}", bid, ask)
        }
        _ => "{\"ok\":false,\"error\":\"UnknownCommand\"}".to_string(),
    }
}

fn main() {
    let mut engine = ExchangeEngine::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = handle_command(&mut engine, trimmed);
        let _ = writeln!(stdout, "{}", response);
        let _ = stdout.flush();
    }
}
