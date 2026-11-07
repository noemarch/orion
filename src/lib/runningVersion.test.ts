import { describe, expect, test } from "bun:test";
import { runningOrionVersion } from "./runningVersion.ts";

describe("runningOrionVersion", () => {
  test("is the running server's, then the installed binary's", () => {
    expect(runningOrionVersion({ server_version: "0.9.3", binary_version: "0.9.4" }, "0.9.0")).toBe("0.9.3");
    expect(runningOrionVersion({ server_version: null, binary_version: "0.9.4" }, "0.9.0")).toBe("0.9.4");
  });

  test("is the health check's where the update status names none", () => {
    expect(runningOrionVersion({ server_version: null, binary_version: null }, "0.9.0")).toBe("0.9.0");
    expect(runningOrionVersion(null, "0.9.0")).toBe("0.9.0");
  });

  test("is nothing when orion has not said", () => {
    expect(runningOrionVersion(null, null)).toBeNull();
    expect(runningOrionVersion({ server_version: null, binary_version: null }, null)).toBeNull();
  });
});
