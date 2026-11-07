/**
 * orion itself, updated from the app (server/orion-update.ts): the server runs
 * `orion update --handoff` for the orion it talks to, on its own PC.
 *
 * The app's own update checks are gone: Orion has no release channel, it is updated by pulling
 * this repository. What is left here is the orion self-update, which is not the app's business.
 */
export interface OrionUpdateStatus {
  /** false where the server offers no orion update (Windows, a orion that does not answer): the controls stay hidden */
  supported: boolean;
  phase: "idle" | "updating" | "error";
  /** the running orion server, and the orion binary installed beside it */
  server_version: string | null;
  binary_version: string | null;
  /** the installed binary is newer than the running server: an update moves the panes onto it */
  stale: boolean;
  /** what orion printed on the last run, its tail; null before any run and while one runs */
  output: string | null;
  finished_at: string | null;
}
