import { expect, it } from "bun:test";
import { appendFileSync } from "node:fs";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { getJson, postJson } from "./orion-client.ts";
import { startOrion } from "./startOrion.ts";

interface Conversation {
  source: string;
  history_id?: string;
  turns: Array<{ role: string; parts: Array<{ kind: string; text?: string }> }>;
  abandoned?: { count: number; branches: number; summary: string | null } | null;
}

interface Created {
  pane_id: string;
  agent_started: boolean;
}

interface Snapshot {
  snapshot: { panes: Array<{ pane_id: string; agent_status: string }> };
}

function paneStatus(snapshot: Snapshot, paneId: string): string | undefined {
  return snapshot.snapshot.panes.find((pane) => pane.pane_id === paneId)?.agent_status;
}

function texts(conversation: Conversation): string[] {
  return conversation.turns.flatMap((turn) =>
    turn.parts.filter((part) => part.kind === "text" && typeof part.text === "string").map((part) => part.text!),
  );
}

/** A harmless `pi` on PATH: the conformance case reads the seeded store, not a live agent. */
async function fakePi(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "orion-fake-pi-"));
  if (process.platform === "win32") {
    await writeFile(join(dir, "pi.cmd"), "@echo off\r\nexit /b 0\r\n");
  } else {
    const shim = join(dir, "pi");
    await writeFile(shim, "#!/bin/sh\nexit 0\n");
    await chmod(shim, 0o755);
  }
  return dir;
}

/** pi's session folder: `--<cwd without its leading separator, separators as dashes>--`. */
function sessionFolder(cwd: string): string {
  return `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`;
}

/** pi writes one entry per line: an id, a parentId, and whatever the type carries. */
interface Entry {
  id: string;
  parentId: string | null;
  timestamp: string;
  type: string;
  [key: string]: unknown;
}

let clock = 0;
function entry(id: string, parentId: string | null, type: string, extra: Record<string, unknown> = {}): Entry {
  clock += 1;
  return {
    id,
    parentId,
    timestamp: new Date(Date.UTC(2026, 9, 16, 21, 5, clock)).toISOString(),
    type,
    ...extra,
  };
}

function user(id: string, parentId: string, text: string): Entry {
  return entry(id, parentId, "message", {
    message: { role: "user", content: [{ type: "text", text }] },
  });
}

function answer(id: string, parentId: string, text: string): Entry {
  return entry(id, parentId, "message", {
    message: { role: "assistant", stopReason: "stop", content: [{ type: "text", text }] },
  });
}

function call(id: string, parentId: string, callId: string): Entry {
  return entry(id, parentId, "message", {
    message: {
      role: "assistant",
      stopReason: "toolUse",
      content: [{ type: "toolCall", id: callId, name: "read", arguments: { path: "src/app.ts" } }],
    },
  });
}

function result(id: string, parentId: string, callId: string): Entry {
  return entry(id, parentId, "message", {
    message: {
      role: "toolResult",
      toolCallId: callId,
      toolName: "read",
      content: [{ type: "text", text: "export const app = null;" }],
    },
  });
}

/** Writes one session into `store` (the sessions directory), binding it to `cwd`. */
async function seedSession(
  store: string,
  cwd: string,
  id: string,
  body: Entry[],
): Promise<string> {
  const dir = join(store, sessionFolder(cwd));
  await mkdir(dir, { recursive: true });
  const header = {
    type: "session",
    version: 3,
    id,
    timestamp: new Date(Date.UTC(2026, 9, 16, 21, 0, 0)).toISOString(),
    cwd,
  };
  const path = join(dir, `${id}.jsonl`);
  await writeFile(path, [header, ...body].map((line) => JSON.stringify(line)).join("\n") + "\n");
  return path;
}

it("serves pi's live entry-tree branch and the turns a /tree left behind", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "orion-pi-cwd-"));
  const store = await mkdtemp(join(tmpdir(), "orion-pi-store-"));
  const shimDir = await fakePi();
  await seedSession(store, cwd, "pi-live", [
    user("u1", "pi-live", "read the file"),
    call("a1", "u1", "c1"),
    result("r1", "a1", "c1"),
    answer("a2", "r1", "It exports null."),
    // /tree back to the answer: this question and its answer are the branch left behind
    user("u-gone", "a2", "the question a /tree dropped"),
    answer("a-gone", "u-gone", "the abandoned answer"),
    entry("bs", "a2", "branch_summary", {
      fromId: "a-gone",
      summary: "Tried the first branch; it failed.",
    }),
    user("u2", "bs", "the retried question"),
    answer("a-live", "u2", "the answer in play"),
  ]);
  const orion = await startOrion({
    env: {
      PATH: `${shimDir}${delimiter}${process.env.PATH ?? ""}`,
      PI_CODING_AGENT_SESSION_DIR: store,
    },
  });
  try {
    const created = await postJson<Created>(orion, "/api/workspace/create", {
      cwd,
      label: "pi",
      agent: { kind: "pi" },
    });
    expect(created.agent_started).toBe(true);
    try {
      const conversation = await getJson<Conversation>(
        orion,
        `/api/pane/conversation?pane_id=${encodeURIComponent(created.pane_id)}`,
      );
      expect(conversation.source).toBe("pi-transcript");
      expect(conversation.history_id).toBe("pi-pi-live");
      expect(texts(conversation)).toEqual([
        "read the file",
        "It exports null.",
        "the retried question",
        "the answer in play",
      ]);
      // the dropped branch is neither shown nor silently gone
      expect(texts(conversation)).not.toContain("the abandoned answer");
      expect(conversation.abandoned).toEqual({
        count: 2,
        branches: 1,
        summary: "Tried the first branch; it failed.",
      });
      const tool = conversation.turns
        .flatMap((turn) => turn.parts)
        .find((part) => part.kind === "tool") as { output?: string } | undefined;
      expect(tool?.output).toBe("export const app = null;");

      // The entry tree is the pane's status record: a new prompt starts the turn, and the
      // answer that stops for good ends it as done.
      const path = join(store, sessionFolder(cwd), "pi-live.jsonl");
      appendFileSync(path, JSON.stringify(user("u3", "a-live", "one more thing")) + "\n");
      expect(
        texts(
          await getJson<Conversation>(
            orion,
            `/api/pane/conversation?pane_id=${encodeURIComponent(created.pane_id)}`,
          ),
        ),
      ).toContain("one more thing");
      expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), created.pane_id)).toBe(
        "working",
      );

      appendFileSync(path, JSON.stringify(answer("a3", "u3", "done")) + "\n");
      expect(
        texts(
          await getJson<Conversation>(
            orion,
            `/api/pane/conversation?pane_id=${encodeURIComponent(created.pane_id)}`,
          ),
        ),
      ).toContain("done");
      expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), created.pane_id)).toBe(
        "done",
      );
    } finally {
      await postJson(orion, "/api/pane/close", { pane_id: created.pane_id });
    }
  } finally {
    await orion.stop();
    await rm(cwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(store, { recursive: true, force: true }).catch(() => undefined);
    await rm(shimDir, { recursive: true, force: true }).catch(() => undefined);
  }
});

it("finds pi's session store under the agent directory pi is given", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "orion-pi-agent-cwd-"));
  const agentDir = await mkdtemp(join(tmpdir(), "orion-pi-agent-"));
  const shimDir = await fakePi();
  await seedSession(join(agentDir, "sessions"), cwd, "pi-agent", [
    user("u1", "pi-agent", "hello pi"),
    answer("a1", "u1", "hello you"),
  ]);
  const orion = await startOrion({
    env: {
      PATH: `${shimDir}${delimiter}${process.env.PATH ?? ""}`,
      PI_CODING_AGENT_DIR: agentDir,
    },
  });
  try {
    const created = await postJson<Created>(orion, "/api/workspace/create", {
      cwd,
      label: "pi",
      agent: { kind: "pi" },
    });
    expect(created.agent_started).toBe(true);
    try {
      const conversation = await getJson<Conversation>(
        orion,
        `/api/pane/conversation?pane_id=${encodeURIComponent(created.pane_id)}`,
      );
      expect(conversation.source).toBe("pi-transcript");
      expect(texts(conversation)).toEqual(["hello pi", "hello you"]);
      expect(conversation.abandoned).toEqual({ count: 0, branches: 0, summary: null });
    } finally {
      await postJson(orion, "/api/pane/close", { pane_id: created.pane_id });
    }
  } finally {
    await orion.stop();
    await rm(cwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(agentDir, { recursive: true, force: true }).catch(() => undefined);
    await rm(shimDir, { recursive: true, force: true }).catch(() => undefined);
  }
});
