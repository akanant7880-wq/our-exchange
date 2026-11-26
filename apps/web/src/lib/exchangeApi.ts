export const API_BASE = process.env.NEXT_PUBLIC_API_URL ?? "/api";
export const WS_BASE = process.env.NEXT_PUBLIC_WS_URL ?? "ws://localhost:8080";
export const MICRO_USDT = 1_000_000;

export const toTicks = (usd: string): number => {
  const n = Number(usd);
  return Number.isFinite(n) && n > 0 ? Math.round(n * 10) : 0;
};

export const toLots = (btc: string): number => {
  const n = Number(btc);
  return Number.isFinite(n) && n > 0 ? Math.round(n * 10_000) : 0;
};

export const fmtUsd = (micro: number | null | undefined): string =>
  micro == null ? "—" : `$${(micro / MICRO_USDT).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;

export const fmtTicks = (ticks: number): string =>
  `$${(ticks / 10).toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 })}`;

export const fmtLots = (lots: number): string =>
  `${(lots / 10_000).toFixed(4)} BTC`;

export async function postJson<T>(path: string, body: Record<string, unknown>): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  return res.json() as Promise<T>;
}

export async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, { cache: "no-store" });
  return res.json() as Promise<T>;
}
