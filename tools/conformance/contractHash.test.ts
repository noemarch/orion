import { expect, it } from "bun:test";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const frozen = JSON.parse(
  await readFile(join(import.meta.dir, "frozen-contract.json"), "utf8"),
) as Record<string, string>;

for (const [file, expected] of Object.entries(frozen)) {
  it(`shared/${file} matches its frozen contract hash`, async () => {
    const bytes = await readFile(join(import.meta.dir, "..", "..", "shared", file));
    const actual = createHash("sha256").update(bytes).digest("hex");
    expect(actual).toBe(expected);
  });
}
