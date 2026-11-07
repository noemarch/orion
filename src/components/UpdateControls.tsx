import { useEffect, useState } from "react";
import { useOrionUpdate } from "../lib/orionUpdate.ts";
import { runningOrionVersion } from "../lib/runningVersion.ts";
import { lazyChunkFailed, onLazyChunkFailure } from "../lib/lazyChunk.ts";
import "./UpdateControls.css";

import { Button } from "../lib/heroui/orion-components.tsx";
import { useT } from "../lib/i18n.ts";

/**
 * orion itself. `orion update` typed into a pane is refused by orion, and every terminal here is
 * a pane: the server runs it instead and moves the running panes onto the new version.
 */
export function OrionUpdateControls({ enabled, orionVersion }: { enabled: boolean; orionVersion: string | null }) {
  const t = useT();
  const { status, error, busy, request } = useOrionUpdate(enabled);
  const version = runningOrionVersion(status, orionVersion);
  // Windows, an older server, a orion that does not answer: nothing to offer, and the version
  // the health check reported is still read here, since the sidebar no longer carries it
  if (!status?.supported) {
    return version ? <section className="settings-section settings-orion-update">
      <h3>orion</h3>
      <div className="settings-card"><p className="settings-item settings-hint">{t("Running Orion {version}", { version })}</p></div>
    </section> : null;
  }
  const stale = status.stale && !!status.binary_version && !!status.server_version;
  return <section className="settings-section settings-orion-update">
    <h3>orion</h3>
    <div className="settings-card"><div className="settings-item">
    {version && <p className="settings-hint">{t("Running Orion {version}", { version })}</p>}
    {stale && <p className="settings-hint">{t("Orion {installed} is installed, but the running engine is {running}. Updating moves your panes onto the installed version.", { installed: status.binary_version ?? "", running: status.server_version ?? "" })}</p>}
    <p className="settings-hint">{t("Installs the newest Orion on this PC and moves its running panes onto it. Panes and agents keep running, and open terminals reconnect.")}</p>
    <div className="update-actions">
      <Button type="button" variant={stale ? "primary" : "secondary"} disabled={busy} onClick={() => void request()}>{t("Update Orion")}</Button>
    </div>
    {(error || busy) && <p className="settings-hint" role="status">{error ?? t("Updating Orion…")}</p>}
    {/* Orion's own words: what it installed, or why it did not */}
    {!busy && status.output && <pre className="update-output" data-failed={status.phase === "error" || undefined}>{status.output}</pre>}
    </div></div>
  </section>;
}

/**
 * The reload line alone, for a page without the update model: the markdown reader tab
 * (FileReader). A part of the app that failed to load there (lib/lazyChunk.ts) leaves math as its
 * source or a diagram as code until a reload, which this line offers. No status is polled.
 */
export function ChunkFailureNotice() {
  const t = useT();
  const [failed, setFailed] = useState(lazyChunkFailed);
  // subscribed first, then read: a failure between the first render and the subscription is kept
  useEffect(() => { const stop = onLazyChunkFailure(() => setFailed(true)); setFailed(lazyChunkFailed()); return stop; }, []);
  if (!failed) return null;
  return <div className="update-notice is-line is-warning" role="status">
    <span>{t("Part of the app could not load. Save unsent drafts, then reload.")}</span>
    <Button type="button" variant="secondary" onClick={() => window.location.reload()}>{t("Reload app")}</Button>
  </div>;
}
