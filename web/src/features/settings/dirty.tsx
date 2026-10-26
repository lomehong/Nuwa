import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import type { SettingsAreaId } from "./areas";

export interface SettingsDirtyState {
  area: SettingsAreaId | null;
  count: number;
}

interface SettingsDirtyContextValue {
  state: SettingsDirtyState;
  report: (state: SettingsDirtyState) => void;
}

const EMPTY: SettingsDirtyState = { area: null, count: 0 };

const SettingsDirtyContext = createContext<SettingsDirtyContextValue | null>(null);

const noopReport = () => undefined;

/**
 * Lets the settings sub-nav mark the area that holds unsaved drafts. The page
 * reports its state; the shell only reads it, so routing stays in one place.
 */
export function SettingsDirtyProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SettingsDirtyState>(EMPTY);
  const report = useCallback((next: SettingsDirtyState) => {
    setState((current) =>
      current.area === next.area && current.count === next.count ? current : next,
    );
  }, []);
  const value = useMemo(() => ({ state, report }), [state, report]);
  return (
    <SettingsDirtyContext.Provider value={value}>{children}</SettingsDirtyContext.Provider>
  );
}

export function useSettingsDirtyState(): SettingsDirtyState {
  return useContext(SettingsDirtyContext)?.state ?? EMPTY;
}

export function useSettingsDirtyReporter(): (state: SettingsDirtyState) => void {
  return useContext(SettingsDirtyContext)?.report ?? noopReport;
}
