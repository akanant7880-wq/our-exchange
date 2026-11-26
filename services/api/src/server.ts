import * as http from "node:http";
import { WebSocket, WebSocketServer } from "ws";
import { EngineBridge } from "./engineBridge";

const PORT = Number(process.env.PORT ?? 8080);
const bridge = new EngineBridge();

const server = http.createServer(async (req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type");

  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  const sendJson = (status: number, data: unknown) => {
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(JSON.stringify(data));
  };

  const readBody = (): Promise<Record<string, unknown>> =>
    new Promise((resolve, reject) => {
      let raw = "";
      req.on("data", (chunk) => (raw += chunk));
      req.on("end", () => {
        try {
          resolve(raw ? JSON.parse(raw) : {});
        } catch {
          reject(new Error("Invalid JSON body"));
        }
      });
    });

  try {
    await handleRoute(req, sendJson, readBody);
  } catch (err) {
    sendJson(400, { ok: false, error: (err as Error).message });
  }
});

const wss = new WebSocketServer({ server });

function broadcast(event: Record<string, unknown>) {
  const msg = JSON.stringify(event);
  for (const client of wss.clients) {
    if (client.readyState === WebSocket.OPEN) {
      client.send(msg);
    }
  }
}

async function handleRoute(
  req: http.IncomingMessage,
  sendJson: (status: number, data: unknown) => void,
  readBody: () => Promise<Record<string, unknown>>
) {
  const url = req.url ?? "/";

  if (req.method === "GET" && url === "/health") {
    return sendJson(200, { ok: true, service: "our-exchange-api" });
  }

  if (req.method === "GET" && url === "/book") {
    const book = await bridge.send({ cmd: "BOOK_TOP" });
    return sendJson(200, book);
  }

  if (req.method === "GET" && url.startsWith("/account/")) {
    const user_id = Number(url.split("/")[2]);
    const state = await bridge.send({ cmd: "ACCOUNT_STATE", user_id });
    return sendJson(200, state);
  }

  if (req.method === "POST" && url === "/deposit") {
    const body = await readBody();
    const out = await bridge.send({ cmd: "DEPOSIT", ...body });
    return sendJson(out.ok ? 200 : 400, out);
  }

  if (req.method === "POST" && url === "/preview-risk") {
    const body = await readBody();
    const out = await bridge.send({ cmd: "PREVIEW_RISK", ...body });
    return sendJson(out.ok ? 200 : 400, out);
  }

  if (req.method === "POST" && url === "/order") {
    const body = await readBody();
    const out = await bridge.send({ cmd: "PLACE_ORDER", ...body });
    if (out.ok) {
      const book = await bridge.send({ cmd: "BOOK_TOP" });
      broadcast({ type: "ORDER_UPDATE", order: out, book });
    }
    return sendJson(out.ok ? 200 : 400, out);
  }

  if (req.method === "POST" && url === "/cancel") {
    const body = await readBody();
    const out = await bridge.send({ cmd: "CANCEL_ORDER", ...body });
    if (out.ok) {
      const book = await bridge.send({ cmd: "BOOK_TOP" });
      broadcast({ type: "CANCEL_UPDATE", cancel: out, book });
    }
    return sendJson(out.ok ? 200 : 400, out);
  }

  sendJson(404, { ok: false, error: "NotFound" });
}

server.listen(PORT, () => {
  console.log(`API & WebSocket server listening on port ${PORT}`);
});
