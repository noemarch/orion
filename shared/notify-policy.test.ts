import { describe, expect, it } from "bun:test";
import {
  alertsAllow,
  inQuietHours,
  parseAlerts,
  type AlertPrefs,
  type QuietHours,
} from "./notify-policy.ts";

describe("inQuietHours", () => {
  it("is false when quiet hours are disabled or missing", () => {
    expect(inQuietHours(undefined)).toBe(false);
    expect(inQuietHours({ enabled: false, start: "22:00", end: "08:00" })).toBe(false);
    expect(inQuietHours({ enabled: true, start: "", end: "08:00" })).toBe(false);
  });

  it("checks windows crossing midnight", () => {
    const quiet: QuietHours = { enabled: true, start: "22:00", end: "08:00" };
    const dateAt = (hours: number, minutes: number): Date => {
      const d = new Date(2026, 10, 8, hours, minutes, 0);
      return d;
    };
    expect(inQuietHours(quiet, dateAt(22, 0))).toBe(true);
    expect(inQuietHours(quiet, dateAt(23, 30))).toBe(true);
    expect(inQuietHours(quiet, dateAt(0, 0))).toBe(true);
    expect(inQuietHours(quiet, dateAt(7, 59))).toBe(true);
    expect(inQuietHours(quiet, dateAt(8, 0))).toBe(false);
    expect(inQuietHours(quiet, dateAt(12, 0))).toBe(false);
    expect(inQuietHours(quiet, dateAt(21, 59))).toBe(false);
  });

  it("checks intraday windows within the same day", () => {
    const quiet: QuietHours = { enabled: true, start: "13:00", end: "14:30" };
    const dateAt = (hours: number, minutes: number): Date => new Date(2026, 10, 8, hours, minutes, 0);
    expect(inQuietHours(quiet, dateAt(12, 59))).toBe(false);
    expect(inQuietHours(quiet, dateAt(13, 0))).toBe(true);
    expect(inQuietHours(quiet, dateAt(13, 45))).toBe(true);
    expect(inQuietHours(quiet, dateAt(14, 29))).toBe(true);
    expect(inQuietHours(quiet, dateAt(14, 30))).toBe(false);
  });
});

describe("alertsAllow with quiet hours and per-agent rules", () => {
  const daytime = new Date(2026, 10, 8, 14, 0, 0);
  const nighttime = new Date(2026, 10, 8, 23, 0, 0);

  it("suppresses alerts during quiet hours", () => {
    const prefs: AlertPrefs = {
      input: true,
      done: "always",
      quietHours: { enabled: true, start: "22:00", end: "08:00" },
    };
    expect(alertsAllow(prefs, "blocked", null, daytime)).toBe(true);
    expect(alertsAllow(prefs, "blocked", null, nighttime)).toBe(false);
    expect(alertsAllow(prefs, "done", null, nighttime)).toBe(false);
  });

  it("respects per-agent alert overrides", () => {
    const prefs: AlertPrefs = {
      input: true,
      done: "off",
      agentAlerts: {
        grok: { input: false },
        claude: { done: "always" },
      },
    };
    // Default agent: input true, done false
    expect(alertsAllow(prefs, "blocked", "opencode", daytime)).toBe(true);
    expect(alertsAllow(prefs, "done", "opencode", daytime)).toBe(false);

    // Grok: input overridden to false
    expect(alertsAllow(prefs, "blocked", "grok", daytime)).toBe(false);

    // Claude: done overridden to always
    expect(alertsAllow(prefs, "done", "claude", daytime)).toBe(true);
  });
});

describe("parseAlerts", () => {
  it("parses quietHours and agentAlerts safely", () => {
    const parsed = parseAlerts({
      input: true,
      done: "always",
      quietHours: { enabled: true, start: "23:00", end: "07:00" },
      agentAlerts: { codex: { done: "off" } },
    });
    expect(parsed.input).toBe(true);
    expect(parsed.done).toBe("always");
    expect(parsed.quietHours).toEqual({ enabled: true, start: "23:00", end: "07:00" });
    expect(parsed.agentAlerts).toEqual({ codex: { done: "off" } });
  });
});
