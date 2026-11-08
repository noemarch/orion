import { expect, it } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { appendFileSync, existsSync, readFileSync, realpathSync } from "node:fs";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, delimiter, join } from "node:path";
import { getJson, postJson } from "./orion-client.ts";
import { startOrion } from "./startOrion.ts";

const ORBIT_FIXTURE =
  process.env.ORBIT_OMP_FIXTURE ?? "fixtures/omp.jsonl";

interface Conversation {
  source: string;
  history_id?: string;
  turns: Array<{
    role: string;
    parts: Array<{
      kind: string;
      text?: string;
      skill?: { name: string; path?: string; evidence: string; status: string };
    }>;
  }>;
}

interface OmoActivity {
  tasks: Array<{
    id: string;
    title: string;
    status: string;
    turns: number | null;
    tool_calls: number | null;
    tokens: number | null;
  }>;
  runs: Array<{
    id: string;
    name: string;
    status: string;
    waves: Array<Array<{ id: string; label: string; state: string }>>;
  }>;
  server_time: string;
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

/** One `message` line of the senpi shape, as the runtime appends it. */
function messageLine(role: string, content: unknown, stop?: string): string {
  const message: Record<string, unknown> = { role, content };
  if (stop !== undefined) message.stopReason = stop;
  return JSON.stringify({
    type: "message",
    id: randomUUID(),
    timestamp: new Date().toISOString(),
    message,
  });
}

function openConversation(
  orion: Awaited<ReturnType<typeof startOrion>>,
  paneId: string,
): Promise<Conversation> {
  return getJson<Conversation>(
    orion,
    `/api/pane/conversation?pane_id=${encodeURIComponent(paneId)}`,
  );
}

/** A harmless `omo`/`omp` on PATH: the conformance case reads the seeded store, not a live agent. */
async function fakeAgent(command: "omo" | "omp"): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), `orion-fake-${command}-`));
  if (process.platform === "win32") {
    await writeFile(join(dir, `${command}.cmd`), "@echo off\r\nexit /b 0\r\n");
  } else {
    const shim = join(dir, command);
    await writeFile(shim, "#!/bin/sh\nexit 0\n");
    await chmod(shim, 0o755);
  }
  return dir;
}

/** senpi's session folder: `--<cwd without its leading separator, separators as dashes>--`. */
function sessionFolder(cwd: string): string {
  return `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`;
}

/** OmO's project key: the canonical cwd's basename, then the first 12 hex of its SHA-256. */
function projectStateKey(cwd: string): string {
  let absolute = cwd;
  try {
    absolute = realpathSync.native(cwd);
  } catch {
    // a missing path is hashed as given, as OmO does when it cannot canonicalize
  }
  const hash = createHash("sha256").update(absolute).digest("hex").slice(0, 12);
  const name = basename(absolute).replace(/[^\p{L}\p{N}._-]/gu, "_") || "root";
  return `${name}-${hash}`;
}

/** The recorded omp session, bound to `cwd` and written where omo/omp look for it. */
async function seedSession(
  agentDir: string,
  cwd: string,
): Promise<{ id: string; path: string }> {
  const lines = readFileSync(ORBIT_FIXTURE, "utf8")
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => JSON.parse(line) as { type?: string; cwd?: string });
  const header = lines.find((line) => line.type === "session");
  if (header === undefined) throw new Error("the omp fixture has no session header");
  header.cwd = cwd;
  const dir = join(agentDir, "sessions", sessionFolder(cwd));
  await mkdir(dir, { recursive: true });
  const path = join(dir, "fixture.jsonl");
  // a real store's first line is the session header; the fixture's leading title line is a
  // rewrite artifact, and the binding reads the first line only
  const ordered = [header, ...lines.filter((line) => line.type !== "session" && line.type !== "title")];
  await writeFile(path, ordered.map((line) => JSON.stringify(line)).join("\n") + "\n");
  return { id: header.id as string, path };
}

/** One ended background task and one running workflow, both of this session and both recent. */
async function seedActivity(
  agentDir: string,
  cwd: string,
  sessionId: string,
): Promise<void> {
  const stateDir = join(agentDir, "projects", projectStateKey(cwd), "senpi-task");
  const now = Date.now();
  const minutesAgo = (minutes: number): string => new Date(now - minutes * 60_000).toISOString();
  await mkdir(join(stateDir, "tasks"), { recursive: true });
  await writeFile(
    join(stateDir, "tasks", "st_1.json"),
    JSON.stringify({
      task_id: "st_1",
      parent_session_id: sessionId,
      status: "completed",
      task_summary: "probe it",
      started_at: minutesAgo(3),
      terminal_at: minutesAgo(1),
      run_stats: { turns: 2, tool_calls: 3, total_tokens: 50 },
    }),
  );
  await mkdir(join(stateDir, "dag", "runs"), { recursive: true });
  await writeFile(
    join(stateDir, "dag", "runs", "dag_1.json"),
    JSON.stringify({
      runId: "dag_1",
      parentSessionId: sessionId,
      name: "probe flow",
      status: "running",
      startedAt: minutesAgo(2),
      nodes: [
        { id: "a", label: "fetch", state: "completed" },
        { id: "b", label: "write", state: "running" },
      ],
      waves: [{ nodeIds: ["a"] }, { nodeIds: ["b"] }],
    }),
  );
}

function texts(conversation: Conversation): string[] {
  return conversation.turns.flatMap((turn) =>
    turn.parts.filter((part) => part.kind === "text" && typeof part.text === "string").map((part) => part.text!),
  );
}

async function withAgent(
  kind: "omo" | "omp",
  seed: (storeDir: string, cwd: string) => Promise<void>,
  assert: (
    orion: Awaited<ReturnType<typeof startOrion>>,
    paneId: string,
  ) => Promise<void>,
): Promise<void> {
  const cwd = await mkdtemp(join(tmpdir(), `orion-${kind}-cwd-`));
  const home = await mkdtemp(join(tmpdir(), `orion-${kind}-agent-`));
  const shimDir = await fakeAgent(kind);
  // omo reads its session store from OMO_CODING_AGENT_DIR; omp takes no such override, so its
  // agent dir is moved by the home the server process sees, under `~/.omp/agent`.
  const storeDir = kind === "omo" ? home : join(home, ".omp", "agent");
  const env: Record<string, string> = {
    PATH: `${shimDir}${delimiter}${process.env.PATH ?? ""}`,
    ...(kind === "omo"
      ? { OMO_CODING_AGENT_DIR: storeDir }
      : { USERPROFILE: home, HOME: home }),
  };
  await seed(storeDir, cwd);
  const orion = await startOrion({ env });
  try {
    const created = await postJson<Created>(orion, "/api/workspace/create", {
      cwd,
      label: kind,
      agent: { kind },
    });
    expect(created.agent_started).toBe(true);
    try {
      await assert(orion, created.pane_id);
    } finally {
      await postJson(orion, "/api/pane/close", { pane_id: created.pane_id });
    }
  } finally {
    await orion.stop();
    await rm(cwd, { recursive: true, force: true }).catch(() => undefined);
    await rm(home, { recursive: true, force: true }).catch(() => undefined);
    await rm(shimDir, { recursive: true, force: true }).catch(() => undefined);
  }
}

it.skipIf(!existsSync(ORBIT_FIXTURE))(
  "serves OmO's transcript, background tasks and workflows through the gateway",
  async () => {
    let sessionPath = "";
    await withAgent(
      "omo",
      async (agentDir, cwd) => {
        const session = await seedSession(agentDir, cwd);
        sessionPath = session.path;
        await seedActivity(agentDir, cwd, session.id);
      },
      async (orion, paneId) => {
        const conversation = await getJson<Conversation>(
          orion,
          `/api/pane/conversation?pane_id=${encodeURIComponent(paneId)}`,
        );
        expect(conversation.source).toBe("omo-transcript");
        expect(conversation.history_id).toBe(
          "omo-01a14688-9e1b-746f-8a5e-e42818ac0899",
        );
        expect(texts(conversation)).toContain("/skill:probe-skill hello");
        expect(texts(conversation)).toContain("PROBED");
        const skill = conversation.turns
          .flatMap((turn) => turn.parts)
          .find((part) => part.kind === "skill")?.skill;
        expect(skill).toMatchObject({
          name: "probe-skill",
          evidence: "instructions",
          status: "loaded",
        });

        const activity = await getJson<OmoActivity>(
          orion,
          `/api/pane/omo-tasks?pane_id=${encodeURIComponent(paneId)}`,
        );
        expect(typeof activity.server_time).toBe("string");
        expect(activity.tasks).toHaveLength(1);
        expect(activity.tasks[0]).toMatchObject({
          id: "st_1",
          title: "probe it",
          status: "completed",
          turns: 2,
          tool_calls: 3,
          tokens: 50,
        });
        expect(activity.runs).toHaveLength(1);
        expect(activity.runs[0]).toMatchObject({ id: "dag_1", name: "probe flow", status: "running" });
        expect(activity.runs[0]!.waves.map((wave) => wave.map((node) => node.label))).toEqual([
          ["fetch"],
          ["write"],
        ]);

        // The transcript is the pane's status record: a prompt the runtime appends starts the
        // turn, and the answer that stops for good ends it as done.
        appendFileSync(
          sessionPath,
          messageLine("user", [{ type: "text", text: "one more thing" }]) + "\n",
        );
        expect(texts(await openConversation(orion, paneId))).toContain("one more thing");
        expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), paneId)).toBe("working");

        appendFileSync(
          sessionPath,
          messageLine("assistant", [{ type: "text", text: "done" }], "stop") + "\n",
        );
        expect(texts(await openConversation(orion, paneId))).toContain("done");
        expect(paneStatus(await getJson<Snapshot>(orion, "/api/session"), paneId)).toBe("done");
      },
    );
  },
);

it.skipIf(!existsSync(ORBIT_FIXTURE))(
  "serves an omp session from its own agent directory",
  async () => {
    await withAgent(
      "omp",
      async (agentDir, cwd) => {
        await seedSession(agentDir, cwd);
      },
      async (orion, paneId) => {
        const conversation = await getJson<Conversation>(
          orion,
          `/api/pane/conversation?pane_id=${encodeURIComponent(paneId)}`,
        );
        expect(conversation.source).toBe("omp-transcript");
        expect(texts(conversation)).toContain("PROBED");
        // an omp pane keeps no senpi tasks here: the answer is empty, not an error
        const activity = await getJson<OmoActivity>(
          orion,
          `/api/pane/omo-tasks?pane_id=${encodeURIComponent(paneId)}`,
        );
        expect(activity.tasks).toEqual([]);
        expect(activity.runs).toEqual([]);
      },
    );
  },
);
