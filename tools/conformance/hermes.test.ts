import { Database } from "bun:sqlite";
import { expect, it } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { getJson, postJson } from "./orion-client.ts";
import { startOrion } from "./startOrion.ts";

interface Part {
  kind: string;
  text?: string;
  name?: string;
  summary?: string;
  input?: string;
  output?: string;
  error?: boolean;
  skill?: { name?: string; evidence?: string; status?: string; path?: string };
}

interface Conversation {
  source: string;
  history_id?: string;
  metadata?: { model: string | null; reasoning_effort: string | null };
  turns: Array<{ role: string; parts: Part[] }>;
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
  return conversation.turns
    .flatMap((turn) => turn.parts)
    .filter((part) => part.kind === "text" && typeof part.text === "string")
    .map((part) => part.text!);
}

/** A harmless `hermes` on PATH: the conformance case reads the seeded store, not a live agent. */
async function fakeHermes(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "orion-fake-hermes-"));
  if (process.platform === "win32") {
    await writeFile(join(dir, "hermes.cmd"), "@echo off\r\nexit /b 0\r\n");
  } else {
    const shim = join(dir, "hermes");
    await writeFile(shim, "#!/bin/sh\nexit 0\n");
    await chmod(shim, 0o755);
  }
  return dir;
}

/**
 * Hermes's `state.db`: a `sessions` row names the directory it ran in, and its `messages` rows
 * are the chat. Only rows that are active or folded into a compaction are shown.
 */
function createStore(dbPath: string): Database {
  const db = new Database(dbPath, { create: true });
  db.run(
    `CREATE TABLE IF NOT EXISTS sessions (
       id TEXT PRIMARY KEY,
       cwd TEXT,
       model TEXT,
       model_config TEXT,
       started_at REAL NOT NULL,
       input_tokens INTEGER DEFAULT 0,
       output_tokens INTEGER DEFAULT 0,
       reasoning_tokens INTEGER DEFAULT 0
     )`,
  );
  db.run(
    `CREATE TABLE IF NOT EXISTS messages (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       session_id TEXT NOT NULL,
       role TEXT NOT NULL,
       content TEXT,
       tool_call_id TEXT,
       tool_calls TEXT,
       tool_name TEXT,
       reasoning TEXT,
       timestamp REAL NOT NULL,
       active INTEGER NOT NULL DEFAULT 1,
       compacted INTEGER NOT NULL DEFAULT 0
     )`,
  );
  return db;
}

function message(
  db: Database,
  session: string,
  row: {
    role: string;
    content?: string;
    tool_call_id?: string;
    tool_calls?: unknown;
    tool_name?: string;
    reasoning?: string;
    timestamp: number;
    active?: number;
  },
): void {
  db.run(
    "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, tool_name, reasoning, timestamp, active, compacted) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
    [
      session,
      row.role,
      row.content ?? null,
      row.tool_call_id ?? null,
      row.tool_calls === undefined ? null : JSON.stringify(row.tool_calls),
      row.tool_name ?? null,
      row.reasoning ?? null,
      row.timestamp,
      row.active ?? 1,
    ],
  );
}

async function openConversation(
  orion: Awaited<ReturnType<typeof startOrion>>,
  paneId: string,
): Promise<Conversation> {
  return getJson<Conversation>(orion, `/api/pane/conversation?pane_id=${encodeURIComponent(paneId)}`);
}

it("serves Hermes's session store through the gateway", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "orion-hermes-cwd-"));
  const otherCwd = await mkdtemp(join(tmpdir(), "orion-hermes-other-"));
  const home = await mkdtemp(join(tmpdir(), "orion-hermes-home-"));
  const shimDir = await fakeHermes();
  const db = createStore(join(home, "state.db"));
  // Hermes writes `started_at` in seconds; a session is this pane's when it started with it.
  const startedAt = Math.floor(Date.now() / 1000) + 60;
  const now = startedAt;
  db.run(
    "INSERT INTO sessions (id, cwd, model, model_config, started_at) VALUES ('hermes-live', ?, 'claude-3-7-sonnet', ?, ?)",
    [cwd, JSON.stringify({ reasoning_effort: "high" }), startedAt],
  );
  // a session of another directory that started later is not this pane's
  db.run(
    "INSERT INTO sessions (id, cwd, model, model_config, started_at) VALUES ('hermes-foreign', ?, 'm', NULL, ?)",
    [otherCwd, startedAt + 120],
  );
  message(db, "hermes-foreign", {
    role: "user",
    content: "the other directory's question",
    timestamp: now + 1,
  });
  message(db, "hermes-live", { role: "user", content: "Please check git status", timestamp: now + 1 });
  message(db, "hermes-live", {
    role: "assistant",
    reasoning: "User wants repository status. I should run git status.",
    tool_calls: [{ id: "call-1", function: { name: "bash", arguments: JSON.stringify({ command: "git status" }) } }],
    timestamp: now + 2,
  });
  db.close();

  const orion = await startOrion({
    env: {
      HERMES_HOME: home,
      PATH: `${shimDir}${delimiter}${process.env.PATH ?? ""}`,
    },
  });
  try {
    const created = await postJson<Created>(orion, "/api/workspace/create", {
      cwd,
      label: "hermes",
      agent: { kind: "hermes" },
    });
    expect(created.agent_started).toBe(true);
    try {
      const first = await openConversation(orion, created.pane_id);
      expect(first.source).toBe("hermes-transcript");
      expect(first.history_id).toBe(`hermes:hermes-live:${startedAt * 1000}`);
      expect(first.metadata).toEqual({ model: "claude-3-7-sonnet", reasoning_effort: "high" });
      expect(texts(first)).toEqual(["Please check git status"]);
      // the foreign session is neither shown nor silently adopted
      expect(texts(first)).not.toContain("the other directory's question");
      const assistant = first.turns[1];
      expect(assistant.parts[0]).toMatchObject({
        kind: "thinking",
        text: "User wants repository status. I should run git status.",
      });
      expect(assistant.parts[1]).toMatchObject({
        kind: "tool",
        name: "bash",
        summary: "git status",
        input: JSON.stringify({ command: "git status" }),
        output: "",
      });
      // the prompt is in flight: Hermes waits on the tool it called
      expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), created.pane_id)).toBe("working");

      const writer = createStore(join(home, "state.db"));
      message(writer, "hermes-live", {
        role: "tool",
        content: JSON.stringify({ output: "On branch main\nnothing to commit", exit_code: 0 }),
        tool_call_id: "call-1",
        tool_name: "bash",
        timestamp: now + 3,
      });
      message(writer, "hermes-live", {
        role: "assistant",
        tool_calls: [{ id: "call-2", function: { name: "bash", arguments: JSON.stringify({ command: "exit 3" }) } }],
        timestamp: now + 4,
      });
      message(writer, "hermes-live", {
        role: "tool",
        content: JSON.stringify({ output: "boom", exit_code: 3 }),
        tool_call_id: "call-2",
        tool_name: "bash",
        timestamp: now + 5,
      });
      writer.close();

      const withTools = await openConversation(orion, created.pane_id);
      const tools = withTools.turns[1].parts.filter((part) => part.kind === "tool");
      expect(tools[0]).toMatchObject({ output: "On branch main\nnothing to commit" });
      expect(tools[0]?.error).toBeUndefined();
      expect(tools[1]).toMatchObject({ output: "boom", error: true });
      // a result that landed still asks the model for the next step
      expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), created.pane_id)).toBe("working");

      const finisher = createStore(join(home, "state.db"));
      message(finisher, "hermes-live", {
        role: "assistant",
        content: "Working tree is clean.",
        timestamp: now + 6,
      });
      finisher.close();

      const finished = await openConversation(orion, created.pane_id);
      expect(texts(finished)).toContain("Working tree is clean.");
      // the answer that stops without a call to wait on ends the turn as done
      expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), created.pane_id)).toBe("done");
    } finally {
      await postJson(orion, "/api/pane/close", { pane_id: created.pane_id });
    }
  } finally {
    await orion.stop();
    await rm(cwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(otherCwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(home, { recursive: true, force: true }).catch(() => undefined);
    await rm(shimDir, { recursive: true, force: true }).catch(() => undefined);
  }
});

const ORBIT_FIXTURE = process.env.ORBIT_HERMES_FIXTURE ?? "fixtures/hermes.txt";

it.skipIf(!existsSync(ORBIT_FIXTURE))("chips a Hermes skill invocation with the request the user typed", async () => {
  const prompt = readFileSync(ORBIT_FIXTURE, "utf8");
  const cwd = await mkdtemp(join(tmpdir(), "orion-hermes-skill-cwd-"));
  const home = await mkdtemp(join(tmpdir(), "orion-hermes-skill-home-"));
  const shimDir = await fakeHermes();
  const db = createStore(join(home, "state.db"));
  const startedAt = Math.floor(Date.now() / 1000) + 60;
  db.run(
    "INSERT INTO sessions (id, cwd, model, model_config, started_at) VALUES ('hermes-skill', ?, 'm', NULL, ?)",
    [cwd, startedAt],
  );
  message(db, "hermes-skill", { role: "user", content: prompt, timestamp: startedAt + 1 });
  db.close();

  const orion = await startOrion({
    env: {
      HERMES_HOME: home,
      PATH: `${shimDir}${delimiter}${process.env.PATH ?? ""}`,
    },
  });
  try {
    const created = await postJson<Created>(orion, "/api/workspace/create", {
      cwd,
      label: "hermes skill",
      agent: { kind: "hermes" },
    });
    expect(created.agent_started).toBe(true);
    try {
      const conversation = await openConversation(orion, created.pane_id);
      expect(texts(conversation)).toEqual(["/probe-skill hello"]);
      const skill = conversation.turns[0].parts.find((part) => part.kind === "skill");
      expect(skill?.skill).toEqual({
        name: "probe-skill",
        evidence: "instructions",
        status: "loaded",
        path: "/tmp/ulw-allagent.Oyfi/tmp-3/hermes-home/skills/probe-skill/SKILL.md",
      });
    } finally {
      await postJson(orion, "/api/pane/close", { pane_id: created.pane_id });
    }
  } finally {
    await orion.stop();
    await rm(cwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(home, { recursive: true, force: true }).catch(() => undefined);
    await rm(shimDir, { recursive: true, force: true }).catch(() => undefined);
  }
});
