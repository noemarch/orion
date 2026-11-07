/**
 * The pipe a session runs on.
 *
 * On a browser the pipe is the gateway's WebSocket; inside the desktop shell it is a Tauri
 * IPC channel. [`OrionSocket`](./ws.ts) speaks one protocol over either, so the reconnect,
 * attach and flow-control logic is written once. Nothing above this file may test for the
 * host: `createPipe` is the only place that decides.
 */

/** Matches the DOM's own numbering, so `readyState` reads the same on both pipes. */
export const PIPE_CONNECTING = 0;
export const PIPE_OPEN = 1;
export const PIPE_CLOSING = 2;
export const PIPE_CLOSED = 3;

export interface Pipe {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number): void;
  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "message", listener: (event: { data: string }) => void): void;
  addEventListener(type: "close", listener: (event: { code: number }) => void): void;
  addEventListener(type: "error", listener: () => void): void;
}

/** A socket to the browser gateway. One event object per listener, never the DOM's own. */
class WsPipe implements Pipe {
  private readonly socket: WebSocket;

  constructor(url: string) {
    this.socket = new WebSocket(url);
  }

  get readyState(): number {
    return this.socket.readyState;
  }

  send(data: string): void {
    this.socket.send(data);
  }

  close(code?: number): void {
    this.socket.close(code);
  }

  addEventListener(type: "open" | "message" | "close" | "error", listener: ((event: { data: string }) => void) | ((event: { code: number }) => void) | (() => void)): void {
    if (type === "message") {
      this.socket.addEventListener("message", (event) => (listener as (event: { data: string }) => void)({ data: String(event.data) }));
    } else if (type === "close") {
      this.socket.addEventListener("close", (event) => (listener as (event: { code: number }) => void)({ code: event.code }));
    } else if (type === "open") {
      this.socket.addEventListener("open", () => (listener as () => void)());
    } else {
      this.socket.addEventListener("error", () => (listener as () => void)());
    }
  }
}

export interface TauriChannel {
  onmessage: ((frame: unknown) => void) | null;
}

export interface TauriCore {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  Channel: new <T>() => TauriChannel;
}

/** The shell's IPC surface. Loaded lazily: a browser bundle must not import it. */
export async function loadTauriCore(): Promise<TauriCore> {
  return (await import("@tauri-apps/api/core")) as unknown as TauriCore;
}

type Listeners = {
  open: Set<() => void>;
  message: Set<(event: { data: string }) => void>;
  close: Set<(event: { code: number }) => void>;
  error: Set<() => void>;
};

/**
 * A session over Tauri IPC. `orion_connect` hands the engine a channel and returns the
 * connection id; every later frame names that id, so two windows cannot steal each other's
 * subscription. The engine sends whole `ServerMessage` objects; this pipe hands the socket
 * their JSON, exactly as a WebSocket frame would arrive.
 */
class TauriPipe implements Pipe {
  private readyStateValue = PIPE_CONNECTING;
  private connection: string | null = null;
  private core: TauriCore | null = null;
  private closed = false;
  private readonly listeners: Listeners = {
    open: new Set(),
    message: new Set(),
    close: new Set(),
    error: new Set(),
  };

  constructor() {
    void this.connect();
  }

  get readyState(): number {
    return this.readyStateValue;
  }

  private async connect(): Promise<void> {
    try {
      const core = await loadTauriCore();
      this.core = core;
      const channel = new core.Channel<unknown>();
      channel.onmessage = (frame) => this.deliver(frame);
      const connection = await core.invoke<string>("orion_connect", { onEvent: channel });
      if (this.closed) {
        void core.invoke("orion_close", { connection }).catch(() => undefined);
        return;
      }
      this.connection = connection;
      this.readyStateValue = PIPE_OPEN;
      for (const listener of this.listeners.open) listener();
    } catch {
      this.fail(1006);
    }
  }

  private deliver(frame: unknown): void {
    if (this.closed) return;
    const data = typeof frame === "string" ? frame : JSON.stringify(frame);
    for (const listener of this.listeners.message) listener({ data });
  }

  private fail(code: number): void {
    if (this.closed) return;
    this.closed = true;
    this.readyStateValue = PIPE_CLOSED;
    for (const listener of this.listeners.error) listener();
    for (const listener of this.listeners.close) listener({ code });
  }

  send(data: string): void {
    const core = this.core;
    const connection = this.connection;
    if (core === null || connection === null || this.readyStateValue !== PIPE_OPEN) {
      throw new Error("the desktop session is not open");
    }
    void core.invoke("orion_send", { connection, payload: data }).catch(() => this.fail(1006));
  }

  close(): void {
    if (this.closed) return;
    const core = this.core;
    const connection = this.connection;
    this.closed = true;
    this.readyStateValue = PIPE_CLOSED;
    if (core !== null && connection !== null) {
      void core.invoke("orion_close", { connection }).catch(() => undefined);
    }
    // The socket's own close listeners must see the shutdown, but a caller that disposed first
    // has already stopped listening for a reconnect.
    for (const listener of this.listeners.close) listener({ code: 1000 });
  }

  addEventListener(type: "open" | "message" | "close" | "error", listener: ((event: { data: string }) => void) | ((event: { code: number }) => void) | (() => void)): void {
    if (type === "message") this.listeners.message.add(listener as (event: { data: string }) => void);
    else if (type === "close") this.listeners.close.add(listener as (event: { code: number }) => void);
    else if (type === "open") this.listeners.open.add(listener as () => void);
    else this.listeners.error.add(listener as () => void);
  }
}

/** Whether this page runs inside the desktop shell rather than a browser. */
export function isDesktopShell(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Opens the pipe for one session: Tauri IPC in the desktop shell, the gateway WebSocket in a
 * browser or the installed PWA. `url` is ignored on desktop, where there is no listening port.
 */
export function createPipe(url: string): Pipe {
  return isDesktopShell() ? new TauriPipe() : new WsPipe(url);
}
