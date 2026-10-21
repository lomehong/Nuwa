import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, RouterProvider } from "react-router-dom";

import { ToastProvider } from "../components/ui";
import { ApiError } from "../lib/api";
import { AuthProvider } from "../lib/auth";
import { ErrorBoundary } from "./ErrorBoundary";
import { appRoutes } from "./router";

/**
 * App-wide server-state defaults. Individual pages tune their polling
 * intervals (overview 10s, issues 15s, events 30s, trends 60s); TanStack Query
 * pauses intervals while the tab is hidden by default.
 */
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 10_000,
      refetchOnWindowFocus: false,
      // Retry transport/5xx failures once, but never retry client errors:
      // a 401 must reach the auth state, and 403/404/429 will not change.
      retry: (failureCount, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
          return false;
        }
        return failureCount < 1;
      },
    },
    mutations: {
      retry: false,
    },
  },
});

/**
 * Data router (createBrowserRouter) built from the shared route table.
 *
 * WHY: the settings leave guard uses `useBlocker`, which only exists on a data
 * router. It intercepts every in-app navigation — link clicks (PUSH/REPLACE),
 * the command palette's `navigate()` (PUSH) and browser back/forward (POP) —
 * with one confirmation, whereas the declarative `BrowserRouter` could only
 * catch anchor clicks. The router is a module-level singleton so its history
 * subscription lives for the app's lifetime.
 */
const router = createBrowserRouter(appRoutes);

export default function App() {
  return (
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <ToastProvider>
          <AuthProvider>
            <RouterProvider router={router} />
          </AuthProvider>
        </ToastProvider>
      </QueryClientProvider>
    </ErrorBoundary>
  );
}
