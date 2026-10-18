import { useSyncExternalStore } from "react";

/**
 * Transport reachability observable.
 *
 * `apiFetch` reports whether the last request reached the server at all: any
 * HTTP response (including 4xx/5xx) means the server is reachable, while a
 * rejected fetch means the transport failed. The shell renders an offline
 * banner from this store. This is deliberately narrower than
 * `navigator.onLine`: a reachable network with a dead `nuwa serve` still shows
 * the banner.
 */

type Listener = () => void;

let reachable = true;
const listeners = new Set<Listener>();

function emit(): void {
  for (const listener of listeners) {
    listener();
  }
}

export function reportServerReachable(): void {
  if (!reachable) {
    reachable = true;
    emit();
  }
}

export function reportServerUnreachable(): void {
  if (reachable) {
    reachable = false;
    emit();
  }
}

export function getServerReachable(): boolean {
  return reachable;
}

export function subscribeServerReachability(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Test hook: forget the last transport outcome between test cases. */
export function resetConnectivity(): void {
  reachable = true;
}

export function useServerReachable(): boolean {
  return useSyncExternalStore(subscribeServerReachability, getServerReachable, () => true);
}
