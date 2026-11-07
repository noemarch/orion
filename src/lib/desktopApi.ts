/**
 * The desktop REST surface.
 *
 * The shell has no HTTP server: the page is loaded from the bundle, so every `/api/*` call the
 * client makes has to reach the engine some other way. This module installs two shims, once,
 * before React mounts:
 *
 * - `fetch` answers `/api/*` on this page's origin by sending one `http` request over Tauri IPC
 *   (`orion_request`) and wrapping the engine's `{ status, body }` in a real `Response`. The REST
 *   client in `api.ts` therefore runs unchanged on desktop and in a browser.
 * - `EventSource` listens to `/api/machines/events` over a Tauri channel and re-emits the engine's
 *   push frames as the `MachineEvent`s the app already handles.
 *
 * A browser imports this module too; both shims return early, so the gateway keeps answering
 * `/api` and the DOM's own `EventSource` stays in place.
 */

import type { ServerMessage } from "../../shared/protocol.ts";
import { isDesktopShell, loadTauriCore, type TauriCore } from "./transport.ts";

/** The answer the engine's `http` envelope returns. */
interface HttpAnswer {
  status: number;
  body: unknown;
}

const STATE = "__orionDesktopApi";

interface DesktopWindow extends Window {
  [STATE]?: true;
}

/** Wraps the engine's answer in a `Response` the client's JSON paths can read. */
function respond(status: number, body: unknown): Response {
  const empty = status === 204 || status === 304;
  return new Response(empty ? null : JSON.stringify(body ?? null), {
    status,
    headers: empty ? undefined : { "content-type": "application/json" },
  });
}

function requestBody(body: BodyInit | null | undefined): unknown {
  if (typeof body === "string") {
    try {
      return JSON.parse(body) as unknown;
    } catch {
      return body;
    }
  }
  return null;
}

/** Whether a URL names this page's own `/api` surface. */
function apiTarget(input: RequestInfo | URL): URL | null {
  const raw = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  let url: URL;
  try {
    url = new URL(raw, window.location.href);
  } catch {
    return null;
  }
  return url.origin === window.location.origin && url.pathname.startsWith("/api/") ? url : null;
}

/**
 * The machine-event stream. The engine's connection sends whole `ServerMessage`s; the app only
 * needs the three the roster and alerts act on, wrapped as `machine-message` for machine
 * `local`.
 */
class DesktopEventSource {
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  private core: TauriCore | null = null;
  private connection: string | null = null;
  private closed = false;

  constructor(url: string) {
    if (url.startsWith("/api/machines/events")) void this.open();
  }

  private async open(): Promise<void> {
    try {
      const core = await loadTauriCore();
      this.core = core;
      const channel = new core.Channel<unknown>();
      channel.onmessage = (frame) => this.receive(frame);
      const connection = await core.invoke<string>("orion_connect", { onEvent: channel });
      if (this.closed) {
        void core.invoke("orion_close", { connection }).catch(() => undefined);
        return;
      }
      this.connection = connection;
    } catch (error) {
      if (!this.closed) this.onerror?.(error);
    }
  }

  private receive(frame: unknown): void {
    if (this.closed || typeof frame !== "object" || frame === null) return;
    const message = frame as ServerMessage;
    if (message.type !== "pane-status" && message.type !== "pane-exited" && message.type !== "session-changed") return;
    this.onmessage?.({
      data: JSON.stringify({ type: "machine-message", machine_id: "local", message }),
    });
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    const core = this.core;
    const connection = this.connection;
    if (core !== null && connection !== null) {
      void core.invoke("orion_close", { connection }).catch(() => undefined);
    }
  }

  addEventListener(type: "message", listener: (event: { data: string }) => void): void {
    if (type === "message") this.onmessage = listener;
  }

  removeEventListener(): void {
    this.onmessage = null;
  }
}

/** Installs the shims in the desktop shell. Idempotent: React re-imports nothing, but HMR does. */
export function installDesktopApi(): void {
  if (!isDesktopShell()) return;
  const host = window as DesktopWindow;
  if (host[STATE]) return;
  host[STATE] = true;

  const invoke = async (request: unknown): Promise<unknown> => {
    const core = await loadTauriCore();
    return core.invoke<unknown>("orion_request", { request });
  };

  const originalFetch = window.fetch.bind(window);
  window.fetch = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = apiTarget(input);
    if (url === null) return originalFetch(input, init);
    if (init?.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
    const method = (init?.method ?? (typeof input === "object" && "method" in input ? input.method : "GET")).toUpperCase();
    try {
      const answer = (await invoke({
        type: "http",
        method,
        path: `${url.pathname}${url.search}`,
        body: requestBody(init?.body),
      })) as HttpAnswer;
      if (init?.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");
      return respond(answer.status, answer.body);
    } catch (error) {
      if (error instanceof DOMException) throw error;
      const message = error instanceof Error ? error.message : String(error);
      return respond(500, { error: { code: "desktop_bridge", message } });
    }
  }) as typeof fetch;

  window.EventSource = DesktopEventSource as unknown as typeof EventSource;
}

installDesktopApi();
