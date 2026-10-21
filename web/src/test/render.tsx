import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, type RenderResult } from "@testing-library/react";
import {
  createMemoryRouter,
  RouterProvider,
  type InitialEntry,
} from "react-router-dom";

import { appRoutes } from "../app/router";
import { ToastProvider } from "../components/ui";
import { AuthProvider } from "../lib/auth";

export function createTestQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: false,
        staleTime: Infinity,
        refetchOnWindowFocus: false,
      },
      mutations: {
        retry: false,
      },
    },
  });
}

export type TestRoute = string | { pathname: string; state?: unknown };

/** The memory-router instance type, so tests can drive POP via `navigate(-1)`. */
export type TestRouter = ReturnType<typeof createMemoryRouter>;

/**
 * Render the real route table with test providers.
 *
 * WHY `createMemoryRouter` + `RouterProvider` (a data router) instead of
 * `MemoryRouter`: the app runs on a data router (`createBrowserRouter`), and
 * the settings leave guard uses `useBlocker`, which only works inside a data
 * router. Building the memory router from the SAME `appRoutes` keeps tests
 * faithful to production routing while keeping navigation observable without
 * touching `window.location`. The returned `router` lets a test trigger a POP
 * (`router.navigate(-1)`) to exercise the back-button guard.
 */
export function renderApp(
  route: TestRoute = "/",
): RenderResult & { queryClient: QueryClient; router: TestRouter } {
  const queryClient = createTestQueryClient();
  const router = createMemoryRouter(appRoutes, {
    initialEntries: [route as InitialEntry],
  });
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <AuthProvider>
          <RouterProvider router={router} />
        </AuthProvider>
      </ToastProvider>
    </QueryClientProvider>,
  );
  return { ...result, queryClient, router };
}
