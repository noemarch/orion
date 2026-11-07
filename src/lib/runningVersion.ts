import type { OrionUpdateStatus } from "../../shared/update.ts";

/**
 * The orion version Settings prints: the running server's, else the installed binary's, else
 * the one the health check reported, which is all there is where orion cannot be updated from
 * here (Windows, an older server, the demo).
 */
export function runningOrionVersion(status: Pick<OrionUpdateStatus, "server_version" | "binary_version"> | null, reported: string | null): string | null {
  return status?.server_version || status?.binary_version || reported || null;
}
