import { Navigate, Outlet, Route, Routes, useLocation } from "react-router-dom";

import { AuthErrorState } from "../components/AuthErrorState";
import { AuthLoading } from "../components/AuthLoading";
import { EventsPage } from "../features/events/EventsPage";
import { IssueDetailPage } from "../features/issue-detail/IssueDetailPage";
import { IssuesPage } from "../features/issues/IssuesPage";
import { LoginPage } from "../features/login/LoginPage";
import { OverviewPage } from "../features/overview/OverviewPage";
import { RunnersPage } from "../features/runners/RunnersPage";
import { useAuth } from "../lib/auth";
import { AppShell } from "./AppShell";

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
 * `/issues/:fp` detail, plus the read-only `/runners` and `/events` pages.
 * `/settings` stays a disabled navigation entry until M5; unknown paths return
 * to the overview.
 */
export function AppRoutes() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      <Route element={<RequireAuth />}>
        <Route element={<AppShell />}>
          <Route index element={<OverviewPage />} />
          <Route path="issues" element={<IssuesPage />} />
          <Route path="issues/:fp" element={<IssueDetailPage />} />
          <Route path="runners" element={<RunnersPage />} />
          <Route path="events" element={<EventsPage />} />
        </Route>
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}
