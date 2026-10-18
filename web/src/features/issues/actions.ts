/**
 * Owner actions on an issue: reopen a rejected/escalated issue or requeue a
 * stuck timed-out run (docs/09 §8.5, `crates/nuwa/src/api/actions.rs`).
 *
 * Both endpoints are POSTs through {@link apiFetch}, so the CSRF double-submit
 * header is attached automatically. A successful write invalidates every view
 * the issue can appear in — the list, detail, overview and audit log.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";

import type { IssueState } from "../../components/ui";
import { apiFetch } from "../../lib/api";
import { invalidateIssueViews } from "../../lib/queryInvalidation";

export type IssueAction = "reopen" | "requeue";

/**
 * The state machine decides which owner action is legal; the UI never offers
 * an action the server would reject.
 */
export function issueActionFor(issue: { state: IssueState; stuck: boolean }): IssueAction | null {
  if (issue.state === "escalated" || issue.state === "rejected") {
    return "reopen";
  }
  if ((issue.state === "triggered" || issue.state === "fixing") && issue.stuck) {
    return "requeue";
  }
  return null;
}

export function useReopenIssue(fingerprint: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      apiFetch<void>(`/issues/${encodeURIComponent(fingerprint)}/reopen`, {
        method: "POST",
        body: JSON.stringify({}),
      }),
    onSuccess: () => invalidateIssueViews(queryClient, fingerprint),
  });
}

/**
 * `force` skips the server-side stuck check. The UI only surfaces requeue for
 * issues the server already marks stuck, so callers normally use the default.
 */
export function useRequeueIssue(fingerprint: string, force = false) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      apiFetch<void>(`/issues/${encodeURIComponent(fingerprint)}/requeue`, {
        method: "POST",
        body: JSON.stringify(force ? { force: true } : {}),
      }),
    onSuccess: () => invalidateIssueViews(queryClient, fingerprint),
  });
}
