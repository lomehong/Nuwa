/**
 * Schema-aware validation for the settings form (docs/09 §9.2 settings).
 *
 * This mirrors only the cheap client-side checks (env quoting, int range,
 * enum membership, http(s) URL). The server stays authoritative: its 422 `fields`
 * map is what reaches the operator when anything else fails. An empty value is allowed
 * here because clearing (or keeping a secret) is a server-side rule.
 */

import type { SettingDto } from "./types";

/** A URL with no whitespace and an http/https scheme (docs/09 §13.1). */
const URL_PATTERN = /^https?:\/\/\S+$/i;
const INTEGER_PATTERN = /^-?\d+$/;

export function validateSettingValue(spec: SettingDto, raw: string): string | null {
  if (spec.type === "int" && raw === "") {
    // Ints are not clearable: an empty field would silently fall back to the
    // server default, so require an explicit number.
    return "请输入整数";
  }
  if (raw === "") {
    // An empty non-secret follows the server's default/clear rules; an empty
    // secret means "keep the current value".
    return null;
  }
  if (/["'\\]/.test(raw)) {
    return "不能包含单引号、双引号或反斜杠";
  }
  if (spec.type === "int") {
    if (!INTEGER_PATTERN.test(raw)) {
      return "请输入整数";
    }
    const value = Number(raw);
    if (
      (spec.min !== null && value < spec.min) ||
      (spec.max !== null && value > spec.max)
    ) {
      return `必须在 ${spec.min ?? "…"}–${spec.max ?? "…"} 之间`;
    }
  }
  if (spec.type === "enum" && !spec.options.includes(raw)) {
    return `必须选择 ${spec.options.join(" / ")}`;
  }
  if (spec.type === "url" && !URL_PATTERN.test(raw)) {
    return "必须是 http(s) URL";
  }
  return null;
}
