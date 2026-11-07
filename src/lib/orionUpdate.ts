import { useCallback, useEffect, useRef, useState } from "react";
import type { OrionUpdateStatus } from "../../shared/update.ts";
import { fetchOrionUpdate, requestOrionUpdate } from "./api.ts";
import { usePageVisible } from "./visibility.ts";

/**
 * orion itself, updated from Settings: the server runs `orion update --handoff` on its own PC
 * (server/orion-update.ts). Asked only while Settings is open; an update in progress is
 * followed closely, since it replaces the orion server under the open terminals.
 */
export function useOrionUpdate(enabled: boolean) {
  const [status, setStatus] = useState<OrionUpdateStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [refresh, setRefresh] = useState(0);
  // the component's own lifetime, not the poll's: a page hidden while the request is on its way
  // (a phone app sent to the background) stops the poll, and the answer must still land
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const visible = usePageVisible();
  useEffect(() => {
    if (!enabled) { setStatus(null); setPending(false); setError(null); return; }
    if (!visible) return;
    let timer: ReturnType<typeof setTimeout>;
    let stopped = false;
    async function poll() {
      let delay = 15_000;
      try {
        const next = await fetchOrionUpdate();
        if (!stopped) setStatus((previous) => JSON.stringify(previous) === JSON.stringify(next) ? previous : next);
        if (next.phase === "updating") delay = 1_500;
      } catch { /* an older server has no such route, and a restart must not erase the last answer */ }
      if (!stopped) timer = setTimeout(() => void poll(), delay);
    }
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, [enabled, refresh, visible]);

  const request = useCallback(async () => {
    setPending(true); setError(null);
    try {
      await requestOrionUpdate();
      if (mounted.current) {
        setStatus((previous) => previous ? { ...previous, phase: "updating", output: null } : previous);
        setRefresh((value) => value + 1);
      }
    } catch (err) {
      if (mounted.current) setError(err instanceof Error ? err.message : String(err));
    } finally { if (mounted.current) setPending(false); }
  }, []);
  const busy = pending || status?.phase === "updating";
  return { status, error, busy, request };
}
