import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";

import { ToastProvider } from "../components/ui";
import { ApiError } from "../lib/api";
import { AuthProvider } from "../lib/auth";
import { AppRoutes } from "./router";

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

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <ToastProvider>
        <AuthProvider>
          <BrowserRouter>
            <AppRoutes />
          </BrowserRouter>
        </AuthProvider>
      </ToastProvider>
    </QueryClientProvider>
  );
}
