import { lazy, Suspense, type ReactNode } from "react";
import { Navigate, Outlet, useLocation, type RouteObject } from "react-router-dom";

import { AuthErrorState } from "../components/AuthErrorState";
import { AuthLoading } from "../components/AuthLoading";
import { Skeleton } from "../components/ui";
import { ArchitecturePage } from "../features/architecture/ArchitecturePage";
import { EventsPage } from "../features/events/EventsPage";
import { IssuesPage } from "../features/issues/IssuesPage";
import { LoginPage } from "../features/login/LoginPage";
import { OverviewPage } from "../features/overview/OverviewPage";
import { ProjectPage } from "../features/projects/ProjectPage";
import { ProjectsPage } from "../features/projects/ProjectsPage";
import { RunnersPage } from "../features/runners/RunnersPage";
import { useAuth } from "../lib/auth";
import { AppShell } from "./AppShell";

// The detail/transcript and settings editors dominate the bundle; loading them
// on demand keeps the first console paint small (docs/09 §11.5 performance).
const IssueDetailPage = lazy(() =>
  import("../features/issue-detail/IssueDetailPage").then((module) => ({
    default: module.IssueDetailPage,
  })),
);
const SettingsPage = lazy(() =>
  import("../features/settings/SettingsPage").then((module) => ({
    default: module.SettingsPage,
  })),
);

function RouteFallback() {
  return (
    <div role="status" aria-busy="true" className="space-y-3">
      <span className="sr-only">正在加载页面</span>
      <Skeleton className="h-8 w-48" />
      <Skeleton className="h-64 w-full" />
    </div>
  );
}

function LazyRoute({ children }: { children: ReactNode }) {
  return <Suspense fallback={<RouteFallback />}>{children}</Suspense>;
}

/**
 * Gate for the console routes: `loading` waits for `/auth/me`, an
 * unauthenticated visitor is sent to `/login` with the intended path in
 * router state so the login page can return there.
 */
function RequireAuth() {
  const { status, retry } = useAuth();
  const location = useLocation();

  if (status === "loading") {
    return <AuthLoading />;
  }
  if (status === "error") {
    return <AuthErrorState onRetry={retry} />;
  }
  if (status === "unauthenticated") {
    const from = `${location.pathname}${location.search}${location.hash}`;
    return <Navigate to="/login" replace state={{ from }} />;
  }
  return <Outlet />;
}

/**
 * Route table (docs/09 §10): `/login`, `/` overview, `/issues` list and
 * `/issues/:fp` detail, the read-only `/runners` and `/events` pages, and the
 * schema-driven `/settings` editor. Unknown paths return to the overview.
 *
 * WHY a data-router config (`RouteObject[]`) instead of a `<Routes>` element:
 * both the app (`createBrowserRouter`) and the tests (`createMemoryRouter`)
 * build their router from this one table, and only a data router exposes
 * `useBlocker` — which the settings leave guard needs to intercept PUSH,
 * REPLACE and POP (browser back / command-palette `navigate()`) uniformly. A
 * declarative `<Routes>` router cannot block POP or programmatic navigation.
 */
export const appRoutes: RouteObject[] = [
  { path: "/login", element: <LoginPage /> },
  {
    element: <RequireAuth />,
    children: [
      {
        element: <AppShell />,
        children: [
          { index: true, element: <OverviewPage /> },
          { path: "projects", element: <ProjectsPage /> },
          { path: "projects/:repo", element: <ProjectPage /> },
          { path: "issues", element: <IssuesPage /> },
          {
            path: "issues/:fp",
            element: (
              <LazyRoute>
                <IssueDetailPage />
              </LazyRoute>
            ),
          },
          { path: "runners", element: <RunnersPage /> },
          { path: "events", element: <EventsPage /> },
          { path: "architecture", element: <ArchitecturePage /> },
          {
            path: "settings",
            element: (
              <LazyRoute>
                <SettingsPage />
              </LazyRoute>
            ),
          },
        ],
      },
    ],
  },
  { path: "*", element: <Navigate to="/" replace /> },
];
