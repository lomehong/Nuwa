import { lazy, Suspense, type ReactNode } from "react";
import { Navigate, Outlet, Route, Routes, useLocation } from "react-router-dom";

import { AuthErrorState } from "../components/AuthErrorState";
import { AuthLoading } from "../components/AuthLoading";
import { Skeleton } from "../components/ui";
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
 */
export function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      <Route element={<RequireAuth />}>
        <Route element={<AppShell />}>
          <Route index element={<OverviewPage />} />
          <Route path="projects" element={<ProjectsPage />} />
          <Route path="projects/:repo" element={<ProjectPage />} />
          <Route path="issues" element={<IssuesPage />} />
          <Route path="issues/:fp" element={<LazyRoute><IssueDetailPage /></LazyRoute>} />
          <Route path="runners" element={<RunnersPage />} />
          <Route path="events" element={<EventsPage />} />
          <Route path="settings" element={<LazyRoute><SettingsPage /></LazyRoute>} />
        </Route>
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}
