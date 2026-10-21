import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import { server } from "../../test/msw";
import { createTestQueryClient } from "../../test/render";
import { useTranscriptQuery } from "./queries";
import type { TranscriptMessage } from "./types";

const FP = "fp-transcript";

function message(text: string): TranscriptMessage {
  return {
    seq: 1,
    role: "assistant",
    ts_ms: 1_700_000_000_000,
    blocks: [{ type: "text", text }],
  };
}

/** Renders the run's first text message, or the load/empty status. */
function TranscriptProbe({ runId, page }: { runId: string | null; page: number }) {
  const query = useTranscriptQuery(FP, runId, page);
  if (query.isPending) {
    return <p>loading</p>;
  }
  const first = query.data?.data[0]?.blocks[0];
  return <p>{first && first.type === "text" ? first.text : "empty"}</p>;
}

/**
 * A stable wrapper so a `rerender` that changes only `runId`/`page` keeps the
 * SAME query observer alive — that is what exercises `placeholderData` (the
 * previous query is only handed to the observer that watched it).
 */
function Harness({
  client,
  runId,
  page,
}: {
  client: ReturnType<typeof createTestQueryClient>;
  runId: string | null;
  page: number;
}) {
  return (
    <QueryClientProvider client={client}>
      <TranscriptProbe runId={runId} page={page} />
    </QueryClientProvider>
  );
}

describe("useTranscriptQuery placeholder data", () => {
  it("does not show run A's messages while switching to a pending run B", async () => {
    let releaseB!: () => void;
    const runBGate = new Promise<void>((resolve) => {
      releaseB = resolve;
    });
    server.use(
      http.get("/api/v1/issues/:fp/transcript", async ({ request }) => {
        const runId = new URL(request.url).searchParams.get("run_id");
        if (runId === "run_b") {
          await runBGate;
        }
        return HttpResponse.json({
          data: [message(runId === "run_b" ? "B message" : "A message")],
          meta: { page: 1, page_size: 20, total: 1, truncated: false, empty: false },
        });
      }),
    );

    const client = createTestQueryClient();
    const view = render(<Harness client={client} runId="run_a" page={1} />);
    expect(await screen.findByText("A message")).toBeInTheDocument();

    // Switch runs while B is still in flight. Because the fingerprint+runId
    // differ, placeholderData returns undefined, so the viewer must fall back
    // to its loading state instead of flashing run A's transcript.
    view.rerender(<Harness client={client} runId="run_b" page={1} />);
    expect(screen.queryByText("A message")).not.toBeInTheDocument();
    expect(screen.getByText("loading")).toBeInTheDocument();

    releaseB();
    expect(await screen.findByText("B message")).toBeInTheDocument();
  });

  it("keeps the previous page visible while paging within the same run", async () => {
    let releasePage2!: () => void;
    const page2Gate = new Promise<void>((resolve) => {
      releasePage2 = resolve;
    });
    server.use(
      http.get("/api/v1/issues/:fp/transcript", async ({ request }) => {
        const page = new URL(request.url).searchParams.get("page");
        if (page === "2") {
          await page2Gate;
        }
        return HttpResponse.json({
          data: [message(`page ${page ?? "1"}`)],
          meta: {
            page: Number(page ?? 1),
            page_size: 20,
            total: 40,
            truncated: false,
            empty: false,
          },
        });
      }),
    );

    const client = createTestQueryClient();
    const view = render(<Harness client={client} runId="run_a" page={1} />);
    expect(await screen.findByText("page 1")).toBeInTheDocument();

    // Same run, next page still loading: the previous page stays on screen
    // (this is the pagination case keepPreviousData was meant for).
    view.rerender(<Harness client={client} runId="run_a" page={2} />);
    await waitFor(() => expect(screen.getByText("page 1")).toBeInTheDocument());

    releasePage2();
    expect(await screen.findByText("page 2")).toBeInTheDocument();
  });
});
