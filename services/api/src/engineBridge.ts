import { ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import * as path from "node:path";
import * as readline from "node:readline";

export interface EngineResponse {
  ok: boolean;
  error?: string;
  [key: string]: unknown;
}

export class EngineBridge {
  private proc: ChildProcessWithoutNullStreams;
  private queue: Array<{
    resolve: (res: EngineResponse) => void;
    reject: (err: Error) => void;
  }> = [];

  constructor(binaryPath?: string) {
    const resolvedBin =
      binaryPath ??
      path.resolve(__dirname, "../../matching-engine/target/debug/engine_server");

    this.proc = spawn(resolvedBin, [], { stdio: ["pipe", "pipe", "inherit"] });

    const rl = readline.createInterface({ input: this.proc.stdout });
    rl.on("line", (line) => {
      const next = this.queue.shift();
      if (!next) return;
      try {
        next.resolve(JSON.parse(line) as EngineResponse);
      } catch (err) {
        next.reject(new Error(`Invalid JSON from engine_server: ${line}`));
      }
    });

    this.proc.on("exit", (code) => {
      while (this.queue.length > 0) {
        this.queue.shift()?.reject(new Error(`engine_server exited with code ${code}`));
      }
    });
  }

  public send(payload: Record<string, unknown>): Promise<EngineResponse> {
    return new Promise((resolve, reject) => {
      this.queue.push({ resolve, reject });
      this.proc.stdin.write(`${JSON.stringify(payload)}\n`);
    });
  }

  public close(): void {
    this.proc.kill();
  }
}
