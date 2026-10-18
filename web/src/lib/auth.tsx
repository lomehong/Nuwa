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

import { ApiError, apiFetch, setUnauthorizedHandler } from "./api";

/** `GET /api/v1/auth/me` (docs/09 §6.1). */
export interface MeResponse {
  authenticated: boolean;
  local_mode: boolean;
  expires_at_ms: number | null;
}

/**
 * `error` means the boot check failed for a non-401 reason (transport failure,
 * 5xx): the operator sees a retryable "server unreachable" state instead of a
 * login form that could never succeed.
 */
export type AuthStatus = "loading" | "authenticated" | "unauthenticated" | "error";

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
  /** Re-runs the `/auth/me` boot check after an `error` status. */
  retry: () => void;
}

type AuthBootState = Pick<AuthContextValue, "status" | "localMode" | "expiresAtMs">;

const UNAUTHENTICATED: AuthBootState = {
  status: "unauthenticated",
  localMode: false,
  expiresAtMs: null,
};

const UNREACHABLE: AuthBootState = {
  status: "error",
  localMode: false,
  expiresAtMs: null,
};

const LOADING: AuthBootState = {
  status: "loading",
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
 * the protected route redirects to `/login` without a full page reload and the
 * cached server state is dropped. A boot failure that is not a 401 (transport
 * error, 5xx) becomes `status: "error"` so the shell can offer a retry instead
 * of pretending the operator is logged out.
 */
export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [state, setState] = useState<AuthBootState>({ ...LOADING });

  const loadMe = useCallback(async (isActive: () => boolean) => {
    try {
      const me = await apiFetch<MeResponse>("/auth/me");
      if (isActive()) {
        setState({
          status: "authenticated",
          localMode: me.local_mode,
          expiresAtMs: me.expires_at_ms,
        });
      }
    } catch (error) {
      if (!isActive()) {
        return;
      }
      // Only a 401 means "not logged in". Everything else (offline, proxy
      // error, 5xx) is a server problem, not a credential problem.
      setState(error instanceof ApiError && error.status === 401 ? UNAUTHENTICATED : UNREACHABLE);
    }
  }, []);

  useEffect(() => {
    let active = true;
    void loadMe(() => active);
    return () => {
      active = false;
    };
  }, [loadMe]);

  useEffect(() => {
    const restore = setUnauthorizedHandler(() => {
      queryClient.clear();
      setState(UNAUTHENTICATED);
    });
    return restore;
  }, [queryClient]);

  const retry = useCallback(() => {
    setState({ ...LOADING });
    void loadMe(() => true);
  }, [loadMe]);

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
    try {
      await apiFetch<void>("/auth/logout", { method: "POST" });
    } catch (error) {
      // A 401 here means the session already expired: the desired end state.
      if (!(error instanceof ApiError && error.status === 401)) {
        throw error;
      }
    }
    setState(UNAUTHENTICATED);
    queryClient.clear();
  }, [queryClient]);

  const value = useMemo<AuthContextValue>(
    () => ({ ...state, login, logout, retry }),
    [state, login, logout, retry],
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
