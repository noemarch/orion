import { expect, it } from "bun:test";
import { startOrion } from "./startOrion.ts";

it("a remote client pairs with a code and then uses its device token", async () => {
  const orion = await startOrion({ token: "owner-secret" });
  try {
    // No credentials: the gate holds (health stays open so the client can discover it).
    expect((await fetch(`${orion.baseUrl}/api/health?scope=bridge`)).status).toBe(200);
    expect((await fetch(`${orion.baseUrl}/api/session`)).status).toBe(401);

    // The owner starts a pairing and reads the code.
    const codeResponse = await fetch(`${orion.baseUrl}/api/devices/pairing-code`, {
      method: "POST",
      headers: { authorization: "Bearer owner-secret", "content-type": "application/json" },
      body: "{}",
    });
    expect(codeResponse.status).toBe(200);
    const issued = (await codeResponse.json()) as { code: string };
    expect(issued.code).toMatch(/^\d{6}$/);

    // The phone exchanges the code for its own token.
    const pairResponse = await fetch(`${orion.baseUrl}/api/devices/pair`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code: issued.code, label: "Phone", role: "drive" }),
    });
    expect(pairResponse.status).toBe(200);
    const paired = (await pairResponse.json()) as { token: string; device: { id: string } };
    expect(paired.token.length).toBeGreaterThan(0);

    // The device token opens the same routes the owner token does.
    const session = await fetch(`${orion.baseUrl}/api/session`, {
      headers: { authorization: `Bearer ${paired.token}` },
    });
    expect(session.status).toBe(200);

    // A wrong code is refused.
    const wrong = await fetch(`${orion.baseUrl}/api/devices/pair`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code: "000000", label: "Other", role: "drive" }),
    });
    expect(wrong.status).toBe(401);
  } finally {
    await orion.stop();
  }
}, 30_000);
