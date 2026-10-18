import { useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import { apiFetch, setUnauthorizedHandler } from "./api";

/** `GET /api/v1/auth/me` (docs/09 §6.1). */
export interface MeResponse {
  authenticated: boolean;
  local_mode: boolean;
  expires_at_ms: number | null;
}

export type AuthStatus = "loading" | "authenticated" | "unauthenticated";

export interface AuthContextValue {
  status: AuthStatus;
  /** Local mode: no UI token is configured, so the API is unauthenticated. */
  localMode: boolean;
  /** Session expiry from `/auth/me`; `null` in local mode. */
  expiresAtMs: number | null;
  /** POST the UI token; the server answers with HttpOnly session cookies. */
  login: (token: string) => Promise<void>;
  /** POST /auth/logout (CSRF-protected) and drop cached server state. */
  logout: () => Promise<void>;
}

const UNAUTHENTICATED: Pick<AuthContextValue, "status" | "localMode" | "expiresAtMs"> = {
  status: "unauthenticated",
  localMode: false,
  expiresAtMs: null,
};

const AuthContext = createContext<AuthContextValue | null>(null);

/**
 * Bootstraps the session from `/auth/me` and exposes login/logout.
 *
 * The UI token is only sent to `/auth/login`; it is never persisted in
 * localStorage. Authentication afterwards is the HttpOnly `nuwa_session`
 * cookie set by the server.
 *
 * A 401 from any non-auth endpoint (for example an expired session during a
 * background poll) notifies this provider through `setUnauthorizedHandler`, so
 * the protected route redirects to `/login` without a full page reload.
 */
export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [state, setState] =
    useState<Pick<AuthContextValue, "status" | "localMode" | "expiresAtMs">>({
      status: "loading",
      localMode: false,
      expiresAtMs: null,
    });

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        const me = await apiFetch<MeResponse>("/auth/me");
        if (active) {
          setState({
            status: "authenticated",
            localMode: me.local_mode,
            expiresAtMs: me.expires_at_ms,
          });
        }
      } catch {
        // A 401 is the expected "not logged in" answer. A network failure also
        // lands on the login page, where the next submit reports the error.
        if (active) {
          setState(UNAUTHENTICATED);
        }
      }
    })();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => setUnauthorizedHandler(() => setState(UNAUTHENTICATED)), []);

  const login = useCallback(
    async (token: string) => {
      await apiFetch<void>("/auth/login", {
        method: "POST",
        body: JSON.stringify({ token }),
      });
      // Login succeeded: flip the state before navigation so the protected
      // route does not bounce back to /login while follow-up calls settle.
      setState({ status: "authenticated", localMode: false, expiresAtMs: null });
      await queryClient.invalidateQueries();
    },
    [queryClient],
  );

  const logout = useCallback(async () => {
    await apiFetch<void>("/auth/logout", { method: "POST" });
    setState(UNAUTHENTICATED);
    queryClient.clear();
  }, [queryClient]);

  const value = useMemo<AuthContextValue>(
    () => ({ ...state, login, logout }),
    [state, login, logout],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const value = useContext(AuthContext);
  if (!value) {
    throw new Error("useAuth must be used inside <AuthProvider>");
  }
  return value;
}
