import { expect, it } from "bun:test";
import { startOrion } from "./startOrion.ts";
import { getJson } from "./orion-client.ts";

it("startOrion serves bridge health on an ephemeral port with a temp state dir", async () => {
  const orion = await startOrion();
  try {
    expect(orion.port).toBeGreaterThan(0);
    const health = await getJson<{ auth: { required: boolean; authenticated: boolean } }>(
      orion,
      "/api/health?scope=bridge",
    );
    expect(health.auth.required).toBe(false);
    expect(health.auth.authenticated).toBe(true);
  } finally {
    await orion.stop();
  }
});
