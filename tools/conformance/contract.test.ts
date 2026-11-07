import { expect, it } from "bun:test";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getJson, postJson, putJson } from "./orion-client.ts";
import { startOrion } from "./startOrion.ts";

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

it("orion serve answers the M1 routes over HTTP", async () => {
  const orion = await startOrion();
  try {
    const health = await getJson<{ ok: boolean }>(orion, "/api/health?scope=bridge");
    expect(health.ok).toBe(true);

    const machines = await getJson<{ machines: Array<{ id: string }> }>(orion, "/api/machines");
    expect(machines.machines[0]?.id).toBe("local");

    await putJson(orion, "/api/preferences", { settings: { theme: "dark" } });
    const preferences = await getJson<{ settings: { theme: string } }>(orion, "/api/preferences");
    expect(preferences.settings.theme).toBe("dark");

    const cwd = await mkdtemp(join(tmpdir(), "orion-contract-"));
    const created = await postJson<{
      pane_id: string;
      workspace_id: string;
      agent_started: boolean;
    }>(orion, "/api/workspace/create", {
      cwd,
      label: "contract",
      agent: { kind: "opencode" },
    });
    expect(created.agent_started).toBe(true);
    expect(created.pane_id.length).toBeGreaterThan(0);

    let text = "";
    for (let attempt = 0; attempt < 60; attempt += 1) {
      const read = await getJson<{ read: { text: string } }>(
        orion,
        `/api/pane/read?pane_id=${encodeURIComponent(created.pane_id)}&source=visible&format=text`,
      );
      text = read.read.text;
      if (text.trim().length > 0) break;
      await sleep(500);
    }
    expect(text.trim().length).toBeGreaterThan(0);

    const prompt = await getJson<{ prompt: unknown }>(
      orion,
      `/api/pane/prompt?pane_id=${encodeURIComponent(created.pane_id)}`,
    );
    expect("prompt" in prompt).toBe(true);

    const conversation = await getJson<{ source: string; turns: unknown[] }>(
      orion,
      `/api/pane/conversation?pane_id=${encodeURIComponent(created.pane_id)}`,
    );
    expect(typeof conversation.source).toBe("string");
    expect(Array.isArray(conversation.turns)).toBe(true);

    const closed = await postJson<{ ok: boolean }>(orion, "/api/pane/close", {
      pane_id: created.pane_id,
    });
    expect(closed.ok).toBe(true);

    const missing = await fetch(`${orion.baseUrl}/api/not/a/route`);
    expect(missing.status).toBe(404);
  } finally {
    await orion.stop();
  }
});
