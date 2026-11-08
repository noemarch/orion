import type { AgentStatus } from "../../shared/protocol.ts";
import { ENDED_NOTIFICATION_BODY, paneNotificationTag, statusNotificationBody } from "../../shared/notify-policy.ts";
import { isDesktopShell } from "./transport.ts";
/**
 * Tab alerts: Web Notifications for pane status transitions while this page is open
 * but hidden. A device with a push subscription (lib/push.ts) gets the same alerts from
 * the server instead, and App skips these. Notification exists only in secure contexts
 * (https or localhost) - a plain-http LAN deployment reports "unsupported", and the bell
 * there switches only the in-app alerts.
 *
 * The transition policy is shared with the server (shared/notify-policy.ts).
 */

export { alertStatus, alertsAllow, shouldNotifyStatus } from "../../shared/notify-policy.ts";

export type NotificationState = "unsupported" | "default" | "granted" | "denied";

export function notificationState(): NotificationState {
  if (isDesktopShell()) return "granted";
  if (typeof globalThis.Notification === "undefined") return "unsupported";
  return globalThis.Notification.permission;
}

export async function requestNotificationPermission(): Promise<NotificationState> {
  if (isDesktopShell()) {
    try {
      // Platform-specific Tauri plugin: dynamically imported only within desktop shell.
      const { requestPermission } = await import("@tauri-apps/plugin-notification");
      const permission = await requestPermission();
      return permission === "granted" ? "granted" : permission === "denied" ? "denied" : "default";
    } catch {
      return "granted";
    }
  }
  if (typeof globalThis.Notification === "undefined") return "unsupported";
  const permission = await globalThis.Notification.requestPermission();
  return permission === "granted" ? "granted" : permission === "denied" ? "denied" : "default";
}

let desktopActionRegistered = false;
async function ensureDesktopAction(): Promise<void> {
  if (desktopActionRegistered || !isDesktopShell()) return;
  desktopActionRegistered = true;
  try {
    // Platform-specific Tauri plugin: dynamically imported only within desktop shell.
    const { onAction } = await import("@tauri-apps/plugin-notification");
    await onAction((action) => {
      if (typeof window !== "undefined") {
        window.focus();
        const data = action.notification?.extra as { pane_id?: string; machine_id?: string; workspace_id?: string } | undefined;
        if (data?.pane_id) {
          window.postMessage(
            {
              type: "select-pane",
              pane_id: data.pane_id,
              machine_id: data.machine_id || "local",
              view: "chat",
              workspace_id: data.workspace_id,
            },
            "*",
          );
        }
      }
    });
  } catch {
    desktopActionRegistered = false;
  }
}

/**
 * Shown through the service worker when there is one: Android Chrome has no
 * `new Notification()`, and the worker's notificationclick (public/sw.js) selects the
 * pane. The constructor is the fallback for a page without a worker.
 */
async function show(paneId: string, title: string, body: string, onClick?: () => void, machineId = "local", workspaceId?: string): Promise<void> {
  if (isDesktopShell()) {
    try {
      // Platform-specific Tauri plugin: dynamically imported only within desktop shell.
      const { isPermissionGranted, sendNotification } = await import("@tauri-apps/plugin-notification");
      const granted = await isPermissionGranted();
      if (!granted) return;
      await ensureDesktopAction();
      sendNotification({
        title,
        body,
        extra: { pane_id: paneId, machine_id: machineId, ...(workspaceId ? { workspace_id: workspaceId } : {}) },
      });
      return;
    } catch {
      /* fallback to web notification below */
    }
  }

  if (typeof globalThis.Notification === "undefined") return;
  if (globalThis.Notification.permission !== "granted") return;
  if (typeof document !== "undefined" && !document.hidden) return; // visible tab: the UI already shows it
  const options: NotificationOptions = { body, tag: paneNotificationTag(paneId, machineId), data: { pane_id: paneId, machine_id: machineId, ...(workspaceId ? { workspace_id: workspaceId } : {}) }, icon: "/icons/icon-192.png?v=orion1" };
  try {
    const registration = "serviceWorker" in navigator ? await navigator.serviceWorker.getRegistration() : undefined;
    if (registration?.active) {
      await registration.showNotification(title, options);
      return;
    }
    const notification = new globalThis.Notification(title, options);
    notification.addEventListener("click", () => {
      window.focus();
      onClick?.();
    });
  } catch {
    /* no way to show it here: the sidebar badge still carries the change */
  }
}

/** `workspaceId`: the pane's workspace, as the server's push names it (server/push.ts), so a tap after the pane ends opens it. */
export function showPaneStatusNotification(paneId: string, title: string, status: AgentStatus, onClick?: () => void, machineId = "local", workspaceId?: string): void {
  void show(paneId, title, statusNotificationBody(status), onClick, machineId, workspaceId);
}

export function showPaneEndedNotification(paneId: string, title: string, onClick?: () => void, machineId = "local", workspaceId?: string): void {
  void show(paneId, title, ENDED_NOTIFICATION_BODY, onClick, machineId, workspaceId);
}
