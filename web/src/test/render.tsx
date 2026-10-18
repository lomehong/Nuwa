import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, type RenderResult } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";

import { AppRoutes } from "../app/router";
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

/**
 * Render the real route table with test providers. `MemoryRouter` keeps
 * navigation observable without touching `window.location`.
 */
export function renderApp(route: TestRoute = "/"): RenderResult & { queryClient: QueryClient } {
  const queryClient = createTestQueryClient();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <AuthProvider>
          <MemoryRouter initialEntries={[route]}>
            <AppRoutes />
          </MemoryRouter>
        </AuthProvider>
      </ToastProvider>
    </QueryClientProvider>,
  );
  return { ...result, queryClient };
}
