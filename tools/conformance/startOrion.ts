import { spawn, type ChildProcess } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface StartOrionOptions {
  stateDir?: string;
  token?: string;
}

export interface StartedOrion {
  baseUrl: string;
  port: number;
  stateDir: string;
  stop(): Promise<void>;
}

function repoRoot(): string {
  // tools/conformance/startOrion.ts -> repo root is two directories up.
  return join(import.meta.dir, "..", "..");
}

function binaryPath(): string {
  const root = repoRoot();
  const targetDir = process.env.CARGO_TARGET_DIR ?? join(root, "target");
  const name = process.platform === "win32" ? "orion.exe" : "orion";
  return join(targetDir, "debug", name);
}

export async function startOrion(options: StartOrionOptions = {}): Promise<StartedOrion> {
  const root = repoRoot();
  const binary = binaryPath();
  const stateDir = options.stateDir ?? (await mkdtemp(join(tmpdir(), "orion-conformance-")));
  const createdStateDir = options.stateDir === undefined;

  const child: ChildProcess = spawn(
    binary,
    ["serve", "--host", "127.0.0.1", "--port", "0", "--state-dir", stateDir],
    {
      cwd: root,
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, ...(options.token ? { ORION_TOKEN: options.token } : {}) },
    },
  );

  let stderr = "";
  child.stderr?.setEncoding("utf8");
  child.stderr?.on("data", (chunk: string) => {
    stderr += chunk;
  });

  const ready = await new Promise<{ port: number; url: string }>((resolve, reject) => {
    let buffer = "";
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      const error = new Error(
        `orion serve did not become ready within 30s (binary: ${binary}). ${stderr.trim()}`.trim(),
      );
      child.kill();
      reject(error);
    }, 30_000);

    child.on("error", (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(
        new Error(
          `failed to start ${binary}: ${error.message}. Run \`cargo build -p orion-core\` first.`,
        ),
      );
    });
    child.on("exit", (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new Error(`orion serve exited early (code ${code ?? signal}). ${stderr.trim()}`.trim()));
    });
    child.stdout?.setEncoding("utf8");
    child.stdout?.on("data", (chunk: string) => {
      buffer += chunk;
      let newline = buffer.indexOf("\n");
      while (newline >= 0) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (line.length > 0) {
          try {
            const value = JSON.parse(line) as { event?: string; port?: number; url?: string };
            if (value.event === "ready" && typeof value.port === "number" && typeof value.url === "string") {
              if (settled) return;
              settled = true;
              clearTimeout(timer);
              resolve({ port: value.port, url: value.url });
              return;
            }
          } catch {
            // Not the ready line yet; keep reading.
          }
        }
        newline = buffer.indexOf("\n");
      }
    });
  });

  let stopped = false;
  const stop = async (): Promise<void> => {
    if (stopped) return;
    stopped = true;
    if (child.exitCode === null) {
      child.kill();
      await new Promise<void>((resolve) => {
        child.once("exit", () => resolve());
        // If kill does not take, do not hang the test forever.
        setTimeout(resolve, 5_000).unref?.();
      });
    }
    if (createdStateDir) {
      await rm(stateDir, { recursive: true, force: true }).catch(() => undefined);
    }
  };

  return {
    baseUrl: ready.url,
    port: ready.port,
    stateDir,
    stop,
  };
}
