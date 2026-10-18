/**
 * Same-origin client for the Nuwa console API (`/api/v1`).
 *
 * Contract (docs/09 §6.3, §9.1):
 *   - every request carries cookies (`credentials: "include"`);
 *   - mutating requests echo the readable `nuwa_csrf` cookie in
 *     `X-NUWA-CSRF` (double-submit);
 *   - failures use the shared `{ error: { code, message, fields? } }`
 *     envelope and surface as {@link ApiError};
 *   - a 401 outside the auth endpoints means the session expired, so the
 *     registered unauthorized handler runs (the app redirects to `/login`).
 *
 * The client never sends an `Authorization` header: the UI token is submitted
 * once to `/auth/login` and afterwards only the HttpOnly cookie authenticates.
 */

import { reportServerReachable, reportServerUnreachable } from "./connectivity";

export const API_BASE = "/api/v1";
export const CSRF_COOKIE_NAME = "nuwa_csrf";
export const CSRF_HEADER_NAME = "X-NUWA-CSRF";

/** The shared error body from `crates/nuwa/src/api/mod.rs`. */
export interface ApiErrorEnvelope {
  error: {
    code: string;
    message: string;
    fields?: Record<string, string>;
  };
}

interface ApiErrorOptions {
  fields?: Record<string, string>;
  retryAfterSeconds?: number;
  body?: unknown;
}

/** A non-2xx API response, or a transport/parse failure (status 0). */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly fields?: Record<string, string>;
  /** Parsed `Retry-After` header, in whole seconds (429 rate limits). */
  readonly retryAfterSeconds?: number;
  /**
   * The full parsed JSON body. Kept because some 409s carry data beside the
   * `error` envelope (a settings restart lists `active_runs`, for example).
   */
  readonly body?: unknown;

  constructor(status: number, code: string, message: string, options: ApiErrorOptions = {}) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.fields = options.fields;
    this.retryAfterSeconds = options.retryAfterSeconds;
    this.body = options.body;
  }
}

type UnauthorizedHandler = () => void;

function defaultUnauthorizedHandler(): void {
  if (typeof window !== "undefined") {
    window.location.assign("/login");
  }
}

let unauthorizedHandler: UnauthorizedHandler = defaultUnauthorizedHandler;

/**
 * Replace the 401 handler (the app registers one that clears the auth state so
 * the router can redirect). Returns a function that restores the previous
 * handler; useful for tests and provider cleanup.
 */
export function setUnauthorizedHandler(handler: UnauthorizedHandler | null): () => void {
  const previous = unauthorizedHandler;
  unauthorizedHandler = handler ?? defaultUnauthorizedHandler;
  return () => {
    unauthorizedHandler = previous;
  };
}

/** `/auth/login`, `/auth/me`, `/auth/logout`: a 401 is an expected answer. */
function isAuthPath(path: string): boolean {
  return path === "/auth" || path.startsWith("/auth/");
}

const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

function readCookie(name: string): string | null {
  if (typeof document === "undefined") {
    return null;
  }
  const prefix = `${name}=`;
  for (const part of document.cookie.split(";")) {
    const entry = part.trim();
    if (!entry.startsWith(prefix)) {
      continue;
    }
    const raw = entry.slice(prefix.length);
    try {
      return decodeURIComponent(raw);
    } catch {
      return raw;
    }
  }
  return null;
}

function isStringRecord(value: unknown): value is Record<string, string> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  return Object.values(value).every((entry) => typeof entry === "string");
}

function isErrorEnvelope(value: unknown): value is ApiErrorEnvelope {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const error = (value as { error?: unknown }).error;
  if (typeof error !== "object" || error === null) {
    return false;
  }
  const candidate = error as { code?: unknown; message?: unknown; fields?: unknown };
  return (
    typeof candidate.code === "string" &&
    typeof candidate.message === "string" &&
    (candidate.fields === undefined || isStringRecord(candidate.fields))
  );
}

function parseRetryAfter(raw: string | null): number | undefined {
  if (raw === null) {
    return undefined;
  }
  const seconds = Number.parseInt(raw, 10);
  return Number.isFinite(seconds) && seconds >= 0 ? seconds : undefined;
}

function defaultErrorMessage(status: number): string {
  switch (status) {
    case 401:
      return "未登录或会话已过期";
    case 403:
      return "请求被拒绝";
    case 404:
      return "资源不存在";
    case 429:
      return "请求过于频繁";
    default:
      return status >= 500 ? "服务器错误" : `请求失败（HTTP ${status}）`;
  }
}

async function apiErrorFromResponse(response: Response): Promise<ApiError> {
  const retryAfterSeconds = parseRetryAfter(response.headers.get("retry-after"));
  let code = "http_error";
  let message = defaultErrorMessage(response.status);
  let fields: Record<string, string> | undefined;

  const raw = await response.text().catch(() => "");
  let body: unknown;
  if (raw) {
    try {
      body = JSON.parse(raw);
    } catch {
      // Not JSON (proxy error page, empty body): keep the status fallback.
      body = undefined;
    }
  }
  if (body && isErrorEnvelope(body)) {
    code = body.error.code;
    message = body.error.message;
    fields = body.error.fields;
  }

  return new ApiError(response.status, code, message, { fields, retryAfterSeconds, body });
}

/**
 * Fetch `<API_BASE><path>` and parse the JSON body.
 *
 * `path` must be API-relative (`/overview`, not `https://…`) so the request
 * cannot leave the console origin. `204` and empty bodies resolve to
 * `undefined`; all other 2xx responses must be JSON.
 */
export async function apiFetch<T>(path: string, init: RequestInit = {}): Promise<T> {
  if (!path.startsWith("/") || path.startsWith("//")) {
    throw new Error(`apiFetch path must be API-relative, got ${path}`);
  }

  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  headers.set("Accept", "application/json");
  if (init.body !== undefined && init.body !== null && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  if (!SAFE_METHODS.has(method)) {
    const csrf = readCookie(CSRF_COOKIE_NAME);
    if (csrf) {
      headers.set(CSRF_HEADER_NAME, csrf);
    }
  }

  let response: Response;
  try {
    response = await fetch(`${API_BASE}${path}`, {
      ...init,
      method,
      headers,
      credentials: "include",
    });
  } catch (error) {
    if (error instanceof Error && error.name === "AbortError") {
      throw error;
    }
    reportServerUnreachable();
    throw new ApiError(0, "network_error", "无法连接服务器");
  }

  // Any HTTP answer proves the server is alive, including 4xx/5xx.
  reportServerReachable();

  if (response.status === 401 && !isAuthPath(path)) {
    unauthorizedHandler();
  }

  if (response.status === 204 || response.status === 205) {
    return undefined as T;
  }

  if (!response.ok) {
    throw await apiErrorFromResponse(response);
  }

  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.includes("application/json")) {
    // 200 with an empty body behaves like 204; anything else must be JSON.
    const text = await response.text();
    if (!text) {
      return undefined as T;
    }
    try {
      return JSON.parse(text) as T;
    } catch {
      throw new ApiError(response.status, "invalid_response", "服务器响应格式无法解析");
    }
  }

  try {
    return (await response.json()) as T;
  } catch {
    throw new ApiError(response.status, "invalid_response", "服务器响应格式无法解析");
  }
}
