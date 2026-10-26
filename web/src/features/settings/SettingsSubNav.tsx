import { clsx } from "clsx";
import { NavLink } from "react-router-dom";

import { SETTINGS_AREAS } from "./areas";
import { useSettingsDirtyState } from "./dirty";

interface SettingsSubNavProps {
  onNavigate?: () => void;
}

/**
 * The four settings views. These are routes, not anchors into one long page:
 * the sidebar switches views, and each view has its own save.
 */
export function SettingsSubNav({ onNavigate }: SettingsSubNavProps) {
  const dirty = useSettingsDirtyState();
  return (
    <div
      role="group"
      aria-label="设置区域"
      className="mb-1 ml-[22px] border-l border-border pl-2"
    >
      {SETTINGS_AREAS.map((area) => (
        <NavLink
          key={area.id}
          to={area.path}
          onClick={onNavigate}
          className={({ isActive }) =>
            clsx(
              "flex min-h-8 items-center gap-2 rounded-card px-2 text-caption transition-colors duration-150 ease-out",
              isActive
                ? "bg-raised font-medium text-text"
                : "text-text-secondary hover:bg-raised hover:text-text",
            )
          }
        >
          <span className="min-w-0 truncate">{area.label}</span>
          {dirty.area === area.id && dirty.count > 0 ? (
            <>
              <span
                aria-hidden="true"
                className="ml-auto size-2 shrink-0 rounded-full bg-warning"
              />
              <span className="sr-only">有未保存更改</span>
            </>
          ) : null}
        </NavLink>
      ))}
    </div>
  );
}
