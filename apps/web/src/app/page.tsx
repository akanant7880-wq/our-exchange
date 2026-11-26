"use client";

import { useCallback, useEffect, useState } from "react";
import {
  WS_BASE,
  fmtLots,
  fmtTicks,
  fmtUsd,
  getJson,
  postJson,
  toLots,
  toTicks,
} from "@/lib/exchangeApi";

interface BookState {
  best_bid: [number, number] | null;
  best_ask: [number, number] | null;
}

interface AccountState {
  available: number;
  reserved: number;
  signed_lots: number;
  avg_entry_ticks: number;
  unrealized_pnl: number;
  realized_pnl: number;
}

interface RiskPreview {
  ok: boolean;
  error?: string;
  required_margin?: number;
  has_sufficient_margin?: boolean;
  projected_tp_profit?: number | null;
  projected_sl_loss?: number | null;
  reward_to_risk_bps?: number | null;
  account_risk_bps?: number | null;
}

export default function TradingTerminal() {
  const [userId, setUserId] = useState(1);
  const [side, setSide] = useState<"BUY" | "SELL">("BUY");
  const [orderType, setOrderType] = useState<"LIMIT" | "MARKET">("LIMIT");
  const [price, setPrice] = useState("95000.0");
  const [qty, setQty] = useState("0.1000");
  const [sl, setSl] = useState("94000.0");
  const [tp, setTp] = useState("97500.0");

  const [book, setBook] = useState<BookState>({ best_bid: null, best_ask: null });
  const [account, setAccount] = useState<AccountState | null>(null);
  const [preview, setPreview] = useState<RiskPreview | null>(null);
  const [statusMsg, setStatusMsg] = useState("Ready — Simulated USDT Paper Mode");
  const [wsConnected, setWsConnected] = useState(false);

  const refreshAll = useCallback(async () => {
    try {
      const [b, a] = await Promise.all([
        getJson<BookState & { ok: boolean }>("/book"),
        getJson<AccountState & { ok: boolean }>(`/account/${userId}`),
      ]);
      if (b.ok) setBook({ best_bid: b.best_bid, best_ask: b.best_ask });
      if (a.ok) setAccount(a);
    } catch {
      setStatusMsg("API offline — start node dist/server.js on port 8080");
    }
  }, [userId]);

  useEffect(() => {
    refreshAll();
    const ws = new WebSocket(WS_BASE);
    ws.onopen = () => setWsConnected(true);
    ws.onclose = () => setWsConnected(false);
    ws.onmessage = () => refreshAll();
    return () => ws.close();
  }, [refreshAll]);

  useEffect(() => {
    const entryTicks =
      orderType === "LIMIT"
        ? toTicks(price)
        : (side === "BUY" ? book.best_ask?.[0] : book.best_bid?.[0]) ?? toTicks(price);
    const qtyLots = toLots(qty);
    if (!entryTicks || !qtyLots) return;

    postJson<RiskPreview>("/preview-risk", {
      user_id: userId,
      side,
      price_ticks: entryTicks,
      qty_lots: qtyLots,
      stop_loss_ticks: toTicks(sl),
      take_profit_ticks: toTicks(tp),
    })
      .then(setPreview)
      .catch(() => {});
  }, [userId, side, orderType, price, qty, sl, tp, book, account]);

  const handleDeposit = async () => {
    const res = await postJson<{ ok: boolean; error?: string }>("/deposit", {
      user_id: userId,
      amount: 10_000_000_000,
      key: `faucet-${userId}-${Date.now()}`,
    });
    setStatusMsg(res.ok ? "Credited +10,000.00 Simulated USDT" : `Error: ${res.error}`);
    refreshAll();
  };

  const handleOrder = async () => {
    const res = await postJson<{ ok: boolean; filled_lots?: number; triggered_count?: number; error?: string }>("/order", {
      order_id: Date.now(),
      user_id: userId,
      side,
      order_type: orderType,
      price_ticks: orderType === "LIMIT" ? toTicks(price) : 0,
      qty_lots: toLots(qty),
      stop_loss_ticks: toTicks(sl),
      take_profit_ticks: toTicks(tp),
    });
    if (res.ok) {
      setStatusMsg(`Order accepted — filled ${fmtLots(res.filled_lots ?? 0)}${res.triggered_count ? ` | TP/SL triggered: ${res.triggered_count}` : ""}`);
      refreshAll();
    } else {
      setStatusMsg(`Rejected: ${res.error}`);
    }
  };

  return (
    <main className="min-h-screen bg-slate-950 text-slate-100 p-4 font-sans">
      <div className="max-w-5xl mx-auto space-y-4">
        <header className="flex flex-wrap items-center justify-between gap-3 bg-slate-900 border border-slate-800 rounded-xl p-4">
          <div>
            <h1 className="text-lg font-bold tracking-tight">BTCUSDT Perpetual <span className="text-xs font-normal px-2 py-0.5 rounded bg-amber-500/20 text-amber-300">Paper Trading</span></h1>
            <p className="text-xs text-slate-400">WS: {wsConnected ? "Live" : "Disconnected"} | {statusMsg}</p>
          </div>
          <div className="flex items-center gap-2">
            <select value={userId} onChange={(e) => setUserId(Number(e.target.value))} className="bg-slate-800 border border-slate-700 rounded-lg px-2.5 py-1.5 text-sm">
              <option value={1}>Trader #1 (Taker)</option>
              <option value={2}>Trader #2 (Maker)</option>
            </select>
            <button onClick={handleDeposit} className="bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-medium px-3 py-1.5 rounded-lg">+10,000 USDT</button>
          </div>
        </header>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <section className="bg-slate-900 border border-slate-800 rounded-xl p-4 space-y-4">
            <h2 className="text-sm font-semibold text-slate-300 uppercase">Order Book (Top of Book)</h2>
            <div className="grid grid-cols-2 gap-3 text-sm">
              <div className="bg-rose-950/40 border border-rose-900/50 rounded-lg p-3">
                <div className="text-xs text-rose-400">Best Ask (Sell)</div>
                <div className="font-mono text-base font-bold text-rose-300">{book.best_ask ? fmtTicks(book.best_ask[0]) : "—"}</div>
                <div className="text-xs text-slate-400">{book.best_ask ? fmtLots(book.best_ask[1]) : "0.0000 BTC"}</div>
              </div>
              <div className="bg-emerald-950/40 border border-emerald-900/50 rounded-lg p-3">
                <div className="text-xs text-emerald-400">Best Bid (Buy)</div>
                <div className="font-mono text-base font-bold text-emerald-300">{book.best_bid ? fmtTicks(book.best_bid[0]) : "—"}</div>
                <div className="text-xs text-slate-400">{book.best_bid ? fmtLots(book.best_bid[1]) : "0.0000 BTC"}</div>
              </div>
            </div>
            <h2 className="text-sm font-semibold text-slate-300 uppercase pt-2">Account & Active Position</h2>
            <div className="grid grid-cols-2 gap-2 text-xs bg-slate-950 p-3 rounded-lg border border-slate-800">
              <div><span className="text-slate-400">Available:</span> <span className="font-mono text-slate-100">{fmtUsd(account?.available)}</span></div>
              <div><span className="text-slate-400">Reserved:</span> <span className="font-mono text-amber-300">{fmtUsd(account?.reserved)}</span></div>
              <div><span className="text-slate-400">Position:</span> <span className="font-mono text-slate-100">{account ? fmtLots(account.signed_lots) : "—"}</span></div>
              <div><span className="text-slate-400">Unrealized P&L:</span> <span className={`font-mono ${(account?.unrealized_pnl ?? 0) >= 0 ? "text-emerald-400" : "text-rose-400"}`}>{fmtUsd(account?.unrealized_pnl)}</span></div>
            </div>
          </section>

          <section className="bg-slate-900 border border-slate-800 rounded-xl p-4 space-y-3">
            <div className="grid grid-cols-2 gap-2">
              <button onClick={() => setSide("BUY")} className={`py-2 rounded-lg font-bold text-sm ${side === "BUY" ? "bg-emerald-600 text-white" : "bg-slate-800 text-slate-400"}`}>BUY / LONG</button>
              <button onClick={() => setSide("SELL")} className={`py-2 rounded-lg font-bold text-sm ${side === "SELL" ? "bg-rose-600 text-white" : "bg-slate-800 text-slate-400"}`}>SELL / SHORT</button>
            </div>
            <div className="grid grid-cols-3 gap-2 text-xs">
              <select value={orderType} onChange={(e) => setOrderType(e.target.value as "LIMIT" | "MARKET")} className="bg-slate-800 border border-slate-700 rounded-lg p-2">
                <option value="LIMIT">LIMIT</option>
                <option value="MARKET">MARKET</option>
              </select>
              <input value={price} disabled={orderType === "MARKET"} onChange={(e) => setPrice(e.target.value)} placeholder="Price" className="bg-slate-800 border border-slate-700 rounded-lg p-2 font-mono disabled:opacity-40" />
              <input value={qty} onChange={(e) => setQty(e.target.value)} placeholder="Qty BTC" className="bg-slate-800 border border-slate-700 rounded-lg p-2 font-mono" />
            </div>
            <div className="grid grid-cols-2 gap-2 text-xs">
              <div><label className="text-slate-400">Stop Loss ($)</label><input value={sl} onChange={(e) => setSl(e.target.value)} className="w-full mt-1 bg-slate-800 border border-slate-700 rounded-lg p-2 font-mono" /></div>
              <div><label className="text-slate-400">Take Profit ($)</label><input value={tp} onChange={(e) => setTp(e.target.value)} className="w-full mt-1 bg-slate-800 border border-slate-700 rounded-lg p-2 font-mono" /></div>
            </div>
            <div className="bg-slate-950 border border-slate-800 rounded-lg p-3 text-xs space-y-1">
              <div className="font-semibold text-amber-300">Exness-Style Virtual Risk Preview</div>
              {preview?.ok ? (
                <>
                  <div className="flex justify-between"><span>Required Margin:</span><span className="font-mono">{fmtUsd(preview.required_margin)}</span></div>
                  <div className="flex justify-between text-emerald-400"><span>Projected TP Profit:</span><span className="font-mono">{fmtUsd(preview.projected_tp_profit)}</span></div>
                  <div className="flex justify-between text-rose-400"><span>Projected SL Loss:</span><span className="font-mono">-{fmtUsd(preview.projected_sl_loss)}</span></div>
                  <div className="flex justify-between text-slate-300"><span>R:R / Account Risk:</span><span className="font-mono">{preview.reward_to_risk_bps ? `${(preview.reward_to_risk_bps / 10000).toFixed(2)}R` : "—"} / {preview.account_risk_bps ? `${(preview.account_risk_bps / 100).toFixed(2)}%` : "—"}</span></div>
                </>
              ) : <div className="text-rose-400">{preview?.error ?? "Enter valid order & TP/SL"}</div>}
            </div>
            <button onClick={handleOrder} className={`w-full py-2.5 rounded-lg font-bold text-sm ${side === "BUY" ? "bg-emerald-600 hover:bg-emerald-500" : "bg-rose-600 hover:bg-rose-500"}`}>Place {side} Order</button>
          </section>
        </div>
      </div>
    </main>
  );
}
