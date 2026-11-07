import type { StartedOrion } from "./startOrion.ts";

async function responseJson<T>(response: Response, label: string): Promise<T> {
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw new Error(`${label} failed with ${response.status}: ${text}`);
  }
  return (await response.json()) as T;
}

export async function getJson<T>(orion: StartedOrion, path: string): Promise<T> {
  const response = await fetch(`${orion.baseUrl}${path}`);
  return responseJson<T>(response, `GET ${path}`);
}

export async function postJson<T>(orion: StartedOrion, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${orion.baseUrl}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return responseJson<T>(response, `POST ${path}`);
}

export async function putJson<T>(orion: StartedOrion, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`${orion.baseUrl}${path}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return responseJson<T>(response, `PUT ${path}`);
}
