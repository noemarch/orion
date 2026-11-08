import type { AgentStatus, PaneInfo } from "./engine-types.ts";

/**
 * When a pane is worth interrupting the user for, and what the interruption says.
 * Shared by the browser's tab notifications and the server's web push, so a device
 * with the app open and a phone with it closed fire on exactly the same events.
 */

/** Statuses worth interrupting the user for. `idle`/`working` are the busy baseline. */
export function shouldNotifyStatus(previous: AgentStatus | undefined, next: AgentStatus): boolean {
  if (previous === undefined) return false; // first sighting (app open, new pane): not news
  if (previous === next) return false;
  return next === "blocked" || next === "done";
}

/**
 * The status the alerts take a pane for. One at rest whose turn's background work still runs
 * (`background_wait`, server/background-wait.ts) is working: that work's notice starts the next
 * turn by itself, and the finish is told once, when the work's turn ends.
 */
export function alertStatus(status: AgentStatus, waiting: boolean | undefined): AgentStatus {
  return waiting === true && status !== "working" && status !== "blocked" ? "working" : status;
}

/** The name a pane goes by in the sidebar and in every notification. */
export function paneTitle(pane: Pick<PaneInfo, "pane_id" | "cwd" | "terminal_title" | "terminal_title_stripped">): string {
  return pane.terminal_title_stripped ?? pane.terminal_title ?? pane.cwd ?? pane.pane_id;
}

const STATUS_BODY: Readonly<Record<string, string>> = {
  blocked: "waiting for your input",
  done: "work finished",
};

export function statusNotificationBody(status: AgentStatus): string {
  return STATUS_BODY[status] ?? String(status);
}

export const ENDED_NOTIFICATION_BODY = "terminal ended";

/** One notification slot per pane: a newer one replaces the older, whichever path showed it. */
export function paneNotificationTag(paneId: string, machineId = "local"): string {
  return machineId === "local" ? `orion-pane-${paneId}` : `orion-remote-${encodeURIComponent(machineId)}-${encodeURIComponent(paneId)}`;
}

/**
 * What one device wants to be alerted about. `input`: an agent waiting on the user.
 * `done`: a finished turn: never, only after one that worked a while (`long`, the default:
 * a quick answer is read where it was asked), or every one.
 */
export type DoneAlerts = "off" | "long" | "always";

export interface QuietHours {
  enabled: boolean;
  /** "HH:MM" 24-hour format, e.g. "22:00" */
  start: string;
  /** "HH:MM" 24-hour format, e.g. "08:00" */
  end: string;
}

export type PerAgentAlerts = Record<string, { input?: boolean; done?: DoneAlerts }>;

export interface AlertPrefs {
  input: boolean;
  done: DoneAlerts;
  quietHours?: QuietHours;
  agentAlerts?: PerAgentAlerts;
}
export const DEFAULT_ALERTS: AlertPrefs = { input: true, done: "long" };

/**
 * Whether the given time falls within the configured quiet hours window.
 * Supports windows crossing midnight (e.g. 22:00 to 08:00) and intraday windows.
 */
export function inQuietHours(quiet: QuietHours | undefined, date = new Date()): boolean {
  if (!quiet || !quiet.enabled || !quiet.start || !quiet.end) return false;
  const parseMin = (time: string): number | null => {
    const parts = time.split(":");
    const h = parseInt(parts[0] ?? "", 10);
    const m = parseInt(parts[1] ?? "", 10);
    if (isNaN(h) || isNaN(m) || h < 0 || h > 23 || m < 0 || m > 59) return null;
    return h * 60 + m;
  };
  const startMin = parseMin(quiet.start);
  const endMin = parseMin(quiet.end);
  if (startMin === null || endMin === null) return false;
  const currentMin = date.getHours() * 60 + date.getMinutes();
  if (startMin <= endMin) {
    return currentMin >= startMin && currentMin < endMin;
  }
  return currentMin >= startMin || currentMin < endMin;
}

/** Only known values survive; anything else is the default, so a device never loses its alerts to a typo. */
export function parseAlerts(value: unknown): AlertPrefs {
  const record = typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
  const done = record["done"];
  const quiet = record["quietHours"];
  const agentAlerts = record["agentAlerts"];
  return {
    input: typeof record["input"] === "boolean" ? record["input"] : DEFAULT_ALERTS.input,
    done: done === "off" || done === "long" || done === "always" ? done : DEFAULT_ALERTS.done,
    ...(typeof quiet === "object" && quiet !== null
      ? {
          quietHours: {
            enabled: typeof (quiet as Record<string, unknown>)["enabled"] === "boolean" ? ((quiet as Record<string, unknown>)["enabled"] as boolean) : false,
            start: typeof (quiet as Record<string, unknown>)["start"] === "string" ? ((quiet as Record<string, unknown>)["start"] as string) : "22:00",
            end: typeof (quiet as Record<string, unknown>)["end"] === "string" ? ((quiet as Record<string, unknown>)["end"] as string) : "08:00",
          },
        }
      : {}),
    ...(typeof agentAlerts === "object" && agentAlerts !== null ? { agentAlerts: agentAlerts as PerAgentAlerts } : {}),
  };
}

/** Whether a device with these preferences hears about this status for an agent and time. */
export function alertsAllow(
  prefs: AlertPrefs,
  status: AgentStatus,
  agentKind?: string | null,
  now = new Date(),
): boolean {
  if (inQuietHours(prefs.quietHours, now)) return false;
  if (agentKind && prefs.agentAlerts?.[agentKind]) {
    const override = prefs.agentAlerts[agentKind];
    if (status === "blocked" && typeof override.input === "boolean") return override.input;
    if (status === "done" && typeof override.done === "string") return override.done !== "off";
  }
  if (status === "blocked") return prefs.input;
  if (status === "done") return prefs.done !== "off";
  return false;
}
