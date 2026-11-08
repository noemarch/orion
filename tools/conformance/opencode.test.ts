import { Database } from "bun:sqlite";
import { expect, it } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { getJson, postJson } from "./orion-client.ts";
import { startOrion } from "./startOrion.ts";

interface Conversation {
  source: string;
  history_id?: string;
  turns: Array<{ role: string; parts: Array<{ kind: string; text?: string }> }>;
}

interface Created {
  pane_id: string;
  agent_started: boolean;
}

/** A harmless `opencode` on PATH: the conformance case reads the store, not a live agent. */
async function fakeOpencode(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "orion-fake-opencode-"));
  if (process.platform === "win32") {
    await writeFile(join(dir, "opencode.cmd"), "@echo off\r\nexit /b 0\r\n");
  } else {
    const shim = join(dir, "opencode");
    await writeFile(shim, "#!/bin/sh\nexit 0\n");
    await chmod(shim, 0o755);
  }
  return dir;
}

function createV2Store(dbPath: string, cwd: string): void {
  const db = new Database(dbPath, { create: true });
  db.run(
    "CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text NOT NULL, slug text NOT NULL, directory text NOT NULL, title text NOT NULL, version text NOT NULL, revert text, time_created integer NOT NULL, time_updated integer NOT NULL)",
  );
  db.run(
    "CREATE TABLE session_message (id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL, seq integer NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL)",
  );
  db.run("CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message (session_id, seq)");
  db.run(
    "INSERT INTO session_v2 (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES (?, 'global', 'slug', ?, 'Conformance', '2.0.24', 1792184434000, 1792184434000)",
    ["ses_conformance", cwd],
  );
  const rows: Array<[string, string, string]> = [
    [
      "m1",
      "user",
      JSON.stringify({ time: { created: 1792184434000 }, text: "fixture question", files: [], agents: [] }),
    ],
    [
      "m2",
      "assistant",
      JSON.stringify({
        time: { created: 1792184435000, completed: 1792184436000 },
        content: [{ type: "text", text: "PROBED" }],
        finish: "stop",
        tokens: { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } },
      }),
    ],
    [
      "m3",
      "idle",
      JSON.stringify({ time: { created: 1792184437000 }, outcome: "succeeded" }),
    ],
  ];
  rows.forEach(([id, type, data], index) => {
    db.run(
      "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?, 'ses_conformance', ?, ?, 1792184434000, 1792184434000, ?)",
      [id, type, index + 1, data],
    );
  });
  db.close();
}

function openConversation(orion: Awaited<ReturnType<typeof startOrion>>, paneId: string) {
  return getJson<Conversation>(orion, `/api/pane/conversation?pane_id=${encodeURIComponent(paneId)}`);
}

function texts(conversation: Conversation): string[] {
  return conversation.turns.flatMap((turn) =>
    turn.parts.filter((part) => part.kind === "text" && typeof part.text === "string").map((part) => part.text!),
  );
}

async function withStore(
  seed: (dbPath: string, cwd: string) => void,
  assert: (conversation: Conversation) => void,
): Promise<void> {
  const cwd = await mkdtemp(join(tmpdir(), "orion-opencode-cwd-"));
  const dbDir = await mkdtemp(join(tmpdir(), "orion-opencode-db-"));
  const dbPath = join(dbDir, "opencode.db");
  seed(dbPath, cwd);
  const shimDir = await fakeOpencode();
  const orion = await startOrion({
    env: {
      OPENCODE_DB: dbPath,
      PATH: `${shimDir}${delimiter}${process.env.PATH ?? ""}`,
    },
  });
  try {
    const created = await postJson<Created>(orion, "/api/workspace/create", {
      cwd,
      label: "opencode",
      agent: { kind: "opencode" },
    });
    expect(created.agent_started).toBe(true);
    try {
      assert(await openConversation(orion, created.pane_id));
    } finally {
      await postJson(orion, "/api/pane/close", { pane_id: created.pane_id });
    }
  } finally {
    await orion.stop();
    await rm(cwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(dbDir, { recursive: true, force: true }).catch(() => undefined);
    await rm(shimDir, { recursive: true, force: true }).catch(() => undefined);
  }
}

it("serves a 2.x OpenCode transcript through the gateway", async () => {
  await withStore(createV2Store, (conversation) => {
    expect(conversation.source).toBe("opencode-transcript");
    expect(conversation.history_id).toBe("opencode-ses_conformance");
    expect(texts(conversation)).toContain("fixture question");
    expect(texts(conversation)).toContain("PROBED");
  });
});

const ORBIT_FIXTURE =
  process.env.ORBIT_OPENCODE_FIXTURE ?? "fixtures/opencode.jsonl";

it.skipIf(!existsSync(ORBIT_FIXTURE))(
  "replays the recorded 1.x OpenCode skill invocation",
  async () => {
    const lines = readFileSync(ORBIT_FIXTURE, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    expect(lines.some((line) => line.table === "session")).toBe(true);
    await withStore(
      (dbPath, cwd) => {
        const db = new Database(dbPath, { create: true });
        db.run(
          "CREATE TABLE session (id text PRIMARY KEY, directory text NOT NULL, revert text, time_created integer NOT NULL, time_updated integer NOT NULL)",
        );
        db.run(
          "CREATE TABLE message (id text PRIMARY KEY, session_id text NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL)",
        );
        db.run(
          "CREATE TABLE part (id text PRIMARY KEY, message_id text NOT NULL, session_id text NOT NULL, time_created integer NOT NULL, time_updated integer NOT NULL, data text NOT NULL)",
        );
        for (const line of lines) {
          const row = line.row;
          if (line.table === "session") {
            // The recorded directory is the probe machine's path; bind the session to this pane's.
            db.run(
              "INSERT INTO session (id, directory, revert, time_created, time_updated) VALUES (?, ?, ?, ?, ?)",
              [row.id, cwd, row.revert, row.time_created, row.time_updated],
            );
          } else if (line.table === "message") {
            db.run(
              "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?, ?, ?, ?, ?)",
              [row.id, row.session_id, row.time_created, row.time_updated, row.data],
            );
          } else if (line.table === "part") {
            db.run(
              "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?, ?, ?, ?, ?, ?)",
              [row.id, row.message_id, row.session_id, row.time_created, row.time_updated, row.data],
            );
          }
        }
        db.close();
      },
      (conversation) => {
        expect(conversation.source).toBe("opencode-transcript");
        expect(texts(conversation)).toContain("/probe-skill hello");
        expect(texts(conversation)).toContain("PROBED");
      },
    );
  },
);
