/**
 * Human-safe error presentation shared by every console surface.
 *
 * Panels, pages and mutations all describe the same {@link ApiError}; keeping
 * the formatters here avoids cross-feature imports (`overview` → `issues`).
 * Never render a stack trace or an unparsed body in the console.
 */

import { ApiError } from "./api";

export function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message || "请求失败";
  }
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return "请求失败，请稍后重试";
}

export function describeErrorCode(error: unknown): string | null {
  return error instanceof ApiError ? error.code : null;
}
