import { clsx } from "clsx";
import {
  Activity,
  Cpu,
  Gem,
  LayoutDashboard,
  ListTodo,
  LogOut,
  Menu,
  RefreshCw,
  Search,
  Settings,
  X,
  type LucideIcon,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, NavLink, Outlet, useLocation } from "react-router-dom";

import { OfflineBanner } from "../components/OfflineBanner";
import { ThemeToggle } from "../components/ThemeToggle";
import { Badge, Button, Skeleton, useToast } from "../components/ui";
import { useOverviewQuery } from "../features/overview/queries";
import { ApiError } from "../lib/api";
import { useAuth } from "../lib/auth";
import { useModalBehavior } from "../lib/modal";

interface NavItem {
  label: string;
  icon: LucideIcon;
  /** Present for routes that exist today; absent items are "Soon". */
  to?: string;
  end?: boolean;
  soon?: string;
}

const NAV_ITEMS: readonly NavItem[] = [
  { label: "总览", icon: LayoutDashboard, to: "/", end: true },
  { label: "问题单", icon: ListTodo, to: "/issues" },
  { label: "运行器", icon: Cpu, soon: "运行器页面将在 M4 提供" },
  { label: "活动", icon: Activity, soon: "活动页面将在 M4 提供" },
  { label: "设置", icon: Settings, soon: "设置页面将在 M5 提供" },
];

/** Bottom navigation keeps the four primary destinations (docs/09 §10.1). */
const MOBILE_NAV_ITEMS = NAV_ITEMS.slice(0, 4);

const DRAWER_ID = "nav-drawer";

interface PageMeta {
  title: string;
  breadcrumb: string | null;
}

function pageMeta(pathname: string): PageMeta {
  if (pathname === "/") {
    return { title: "总览", breadcrumb: null };
  }
  if (pathname === "/issues" || pathname === "/issues/") {
    return { title: "问题单", breadcrumb: null };
  }
  if (pathname.startsWith("/issues/")) {
    const raw = pathname.slice("/issues/".length);
    if (!raw) {
      return { title: "问题单", breadcrumb: null };
    }
    let fingerprint = raw;
    try {
      fingerprint = decodeURIComponent(raw);
    } catch {
      // Keep the raw path segment when it is not valid percent-encoding.
    }
    return { title: "问题单详情", breadcrumb: fingerprint };
  }
  return { title: "Nuwa 控制台", breadcrumb: null };
}

const PILL_CLASS =
  "inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full border border-border bg-surface px-2.5 text-caption whitespace-nowrap text-text-secondary";
const PILL_LINK_CLASS = clsx(
  PILL_CLASS,
  "transition-colors hover:border-border-strong hover:text-text",
);

function StatusDot({ tone }: { tone: "success" | "warning" | "danger" }) {
  return (
    <span
      aria-hidden="true"
      className={clsx(
        "size-1.5 shrink-0 rounded-full",
        tone === "success" && "bg-success",
        tone === "warning" && "bg-warning",
        tone === "danger" && "bg-danger",
      )}
    />
  );
}

function ShellBrand() {
  return (
    <>
      <span
        aria-hidden="true"
        className="flex size-9 shrink-0 items-center justify-center rounded-card border border-accent/40 bg-accent/10 text-accent"
      >
        <Gem className="size-4" />
      </span>
      <div className="min-w-0">
        <p className="truncate text-body font-semibold text-text">女娲</p>
        <p className="truncate text-caption text-text-secondary">Nuwa 控制台</p>
      </div>
    </>
  );
}

function SidebarNavList({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <ul className="space-y-1">
      {NAV_ITEMS.map((item) => {
        const Icon = item.icon;
        return (
          <li key={item.label}>
            {item.to ? (
              <NavLink
                to={item.to}
                end={item.end}
                onClick={onNavigate}
                className={({ isActive }) =>
                  clsx(
                    "flex min-h-11 items-center gap-2.5 rounded-card px-3 py-2 text-body transition-colors",
                    isActive
                      ? "bg-accent/10 font-medium text-text"
                      : "text-text-secondary hover:bg-raised hover:text-text",
                  )
                }
              >
                {({ isActive }) => (
                  <>
                    <Icon
                      aria-hidden="true"
                      className={clsx(
                        "size-4 shrink-0",
                        isActive ? "text-accent" : "text-text-muted",
                      )}
                    />
                    <span>{item.label}</span>
                  </>
                )}
              </NavLink>
            ) : (
              <span
                aria-disabled="true"
                title={item.soon}
                className="flex min-h-11 cursor-not-allowed items-center gap-2.5 rounded-card px-3 py-2 text-body text-text-muted"
              >
                <Icon aria-hidden="true" className="size-4 shrink-0" />
                <span className="flex-1">{item.label}</span>
                <span className="rounded-full border border-border px-1.5 py-0.5 text-caption">
                  Soon
                </span>
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

function SidebarFooter({ localMode }: { localMode: boolean }) {
  return (
    <div className="border-t border-border px-5 py-4">
      {localMode ? (
        <p className="text-caption text-text-secondary">本地模式 · API 无鉴权</p>
      ) : (
        <p className="text-caption text-text-secondary">会话由 HttpOnly Cookie 保护</p>
      )}
    </div>
  );
}

/**
 * Compact health strip fed by `/overview` (docs/09 §10.1). It shares the
 * overview query (and key) with the page: the 10s poll pauses while the tab is
 * hidden and refreshes immediately on focus. Values are links only where a
 * page exists today; runner/settings links arrive with M4/M5.
 */
export function HealthStrip() {
  const overview = useOverviewQuery();

  let content: ReactNode;
  if (overview.isPending) {
    content = (
      <div role="status" aria-busy="true" className="flex items-center gap-2">
        <span className="sr-only">正在加载系统健康</span>
        <Skeleton rounded="full" className="h-7 w-24" />
        <Skeleton rounded="full" className="h-7 w-20" />
        <Skeleton rounded="full" className="h-7 w-16" />
        <Skeleton rounded="full" className="h-7 w-20" />
      </div>
    );
  } else if (!overview.data) {
    content = (
      <div className="flex items-center gap-2">
        <span className={PILL_CLASS}>
          <StatusDot tone="danger" />
          serve 不可达
        </span>
        <Button variant="ghost" size="sm" onClick={() => void overview.refetch()}>
          <RefreshCw aria-hidden="true" className="size-3.5" />
          重试
        </Button>
      </div>
    );
  } else {
    const { active_runs: activeRuns, health, queue } = overview.data;
    content = (
      <>
        <span className={PILL_CLASS} title="控制台 API 可达">
          <StatusDot tone="success" />
          serve 正常
        </span>
        <span
          className={PILL_CLASS}
          title={`运行器在线 ${health.runners_online}/${health.runners_total}`}
        >
          <Cpu aria-hidden="true" className="size-3.5 text-text-muted" />
          <span className="sr-only">运行器</span>
          <span>在线</span>
          <span className="font-mono tabular-nums">
            {health.runners_online}/{health.runners_total}
          </span>
        </span>
        <Link
          to="/issues?state=new"
          className={PILL_LINK_CLASS}
          title="查看待领取的问题单"
        >
          <ListTodo aria-hidden="true" className="size-3.5 text-text-muted" />
          <span>队列</span>
          <span className="font-mono tabular-nums">{queue.ready_count}</span>
        </Link>
        <Link
          to="/issues?state=triggered,fixing"
          className={PILL_LINK_CLASS}
          title="查看运行中的问题单"
        >
          <Activity aria-hidden="true" className="size-3.5 text-text-muted" />
          <span>运行中</span>
          <span className="font-mono tabular-nums">{activeRuns.length}</span>
        </Link>
        <span className={PILL_CLASS} title="今日打开的 PR（按仓库配额统计）">
          <span>今日 PR</span>
          <span className="font-mono tabular-nums">{health.prs_today}</span>
        </span>
        {health.standing_down > 0 ? (
          <span className={PILL_CLASS} title="配额用完、进入待命的运行器数量">
            <StatusDot tone="warning" />
            <span>配额待命</span>
            <span className="font-mono tabular-nums">{health.standing_down}</span>
          </span>
        ) : null}
        {overview.isError || overview.isRefetchError ? (
          <button
            type="button"
            onClick={() => void overview.refetch()}
            className={clsx(PILL_LINK_CLASS, "cursor-pointer")}
          >
            <StatusDot tone="danger" />
            更新失败 · 重试
          </button>
        ) : null}
      </>
    );
  }

  return (
    <section aria-label="全局健康条" className="border-b border-border bg-bg">
      <div className="mx-auto flex w-full max-w-page items-center gap-2 overflow-x-auto px-4 py-2 sm:px-6 lg:px-8">
        {content}
      </div>
    </section>
  );
}

/**
 * Application shell: fixed desktop sidebar (232px), sticky topbar with the
 * page title/breadcrumb, theme toggle and logout, the health strip, and a
 * four-slot bottom navigation under 1024px. Under 1024px the full navigation
 * lives in a portaled drawer with focus trapping, Escape/backdrop dismissal,
 * background inert, scroll lock and focus restoration to the menu button.
 */
export function AppShell() {
  const { localMode, logout } = useAuth();
  const { toast } = useToast();
  const location = useLocation();
  const { title, breadcrumb } = pageMeta(location.pathname);
  const [loggingOut, setLoggingOut] = useState(false);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const menuButtonRef = useRef<HTMLButtonElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);
  const drawerCloseRef = useRef<HTMLButtonElement>(null);

  const closeDrawer = useCallback(() => setDrawerOpen(false), []);
  const handleDrawerKeyDown = useModalBehavior({
    active: drawerOpen,
    containerRef: drawerRef,
    initialFocusRef: drawerCloseRef,
    returnFocusRef: menuButtonRef,
    onEscape: closeDrawer,
  });

  // A route change always dismisses the drawer (nav links call close too; this
  // covers browser back/forward and redirects).
  useEffect(() => {
    setDrawerOpen(false);
  }, [location.pathname]);

  // The drawer portal is `lg:hidden`, but the modal lock (inert + body scroll
  // lock) is not: if the viewport grows past 1024px while the drawer is open,
  // it would become invisible with the lock still applied and no visible way
  // to close it. Close as soon as the desktop breakpoint starts matching.
  useEffect(() => {
    if (!drawerOpen || typeof window.matchMedia !== "function") {
      return;
    }
    const query = window.matchMedia("(min-width: 1024px)");
    if (query.matches) {
      closeDrawer();
      return;
    }
    const handleChange = (event: MediaQueryListEvent) => {
      if (event.matches) {
        closeDrawer();
      }
    };
    query.addEventListener("change", handleChange);
    return () => query.removeEventListener("change", handleChange);
  }, [drawerOpen, closeDrawer]);

  async function handleLogout() {
    if (loggingOut) {
      return;
    }
    setLoggingOut(true);
    try {
      await logout();
    } catch (error) {
      toast({
        title: "退出失败",
        description:
          error instanceof ApiError
            ? error.message || "服务器拒绝了退出请求，请稍后重试。"
            : "无法联系服务器，请检查网络后重试。",
        variant: "danger",
      });
    } finally {
      setLoggingOut(false);
    }
  }

  return (
    <div className="min-h-screen bg-bg text-text">
      <a
        href="#main-content"
        onClick={(event) => {
          const main = document.getElementById("main-content");
          if (main) {
            // Keep the URL clean and move focus, not just the scroll position.
            event.preventDefault();
            main.focus();
          }
        }}
        className="sr-only focus:not-sr-only focus:absolute focus:left-4 focus:top-3 focus:z-[60] focus:block focus:rounded-card focus:bg-surface focus:px-4 focus:py-2 focus:text-body focus:text-text"
      >
        跳到主要内容
      </a>

      <aside className="hidden border-r border-border bg-surface lg:fixed lg:inset-y-0 lg:left-0 lg:z-40 lg:flex lg:w-58 lg:flex-col">
        <div className="flex h-16 items-center gap-2.5 border-b border-border px-5">
          <ShellBrand />
        </div>

        <nav aria-label="主导航" className="flex-1 overflow-y-auto px-3 py-4">
          <SidebarNavList />
        </nav>

        <SidebarFooter localMode={localMode} />
      </aside>

      <div className="lg:pl-58">
        <header className="sticky top-0 z-30 border-b border-border bg-bg/95">
          <div className="mx-auto flex h-16 w-full max-w-page items-center gap-2 px-4 sm:gap-3 sm:px-6 lg:px-8">
            <button
              ref={menuButtonRef}
              type="button"
              aria-label="打开导航菜单"
              aria-expanded={drawerOpen}
              aria-controls={DRAWER_ID}
              onClick={() => setDrawerOpen(true)}
              className="flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text lg:hidden"
            >
              <Menu aria-hidden="true" className="size-5" />
            </button>

            <Link
              to="/"
              aria-label="返回总览"
              className="flex shrink-0 items-center gap-2 lg:hidden"
            >
              <span
                aria-hidden="true"
                className="flex size-9 items-center justify-center rounded-card border border-accent/40 bg-accent/10 text-accent"
              >
                <Gem className="size-4" />
              </span>
              <span className="hidden text-body font-semibold text-text sm:inline">女娲</span>
            </Link>

            <div className="min-w-0 flex-1">
              <h1 className="truncate text-heading font-semibold text-text">{title}</h1>
              {breadcrumb ? (
                <p className="truncate font-mono text-caption text-text-secondary">{breadcrumb}</p>
              ) : null}
            </div>

            <div className="hidden md:block">
              <Button
                variant="secondary"
                size="sm"
                disabled
                aria-label="全局搜索（M4 提供）"
                title="全局搜索将在 M4 提供"
                className="w-60 justify-start font-normal text-text-muted"
              >
                <Search aria-hidden="true" className="size-3.5 shrink-0" />
                <span className="truncate">搜索问题单…</span>
                <kbd className="ml-auto rounded border border-border px-1 font-mono text-caption">
                  ⌘K
                </kbd>
              </Button>
            </div>

            <ThemeToggle />

            {localMode ? (
              <Badge variant="warning" title="本地模式：API 无鉴权">
                本地模式
              </Badge>
            ) : (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => void handleLogout()}
                pending={loggingOut}
                aria-label="退出登录"
                title="退出登录"
              >
                <LogOut aria-hidden="true" className="size-4" />
                <span className="hidden sm:inline">退出</span>
              </Button>
            )}
          </div>
        </header>

        <OfflineBanner />

        <HealthStrip />

        <main
          id="main-content"
          tabIndex={-1}
          className="mx-auto w-full max-w-page px-4 py-6 pb-24 sm:px-6 lg:px-8 lg:pb-10"
        >
          <Outlet />
        </main>
      </div>

      {drawerOpen
        ? createPortal(
            <div className="fixed inset-0 z-50 lg:hidden">
              <div
                aria-hidden="true"
                data-testid="nav-drawer-backdrop"
                className="nuwa-backdrop absolute inset-0 bg-scrim"
                onMouseDown={(event) => {
                  if (event.target === event.currentTarget) {
                    closeDrawer();
                  }
                }}
              />
              <div
                ref={drawerRef}
                id={DRAWER_ID}
                role="dialog"
                aria-modal="true"
                aria-label="导航菜单"
                tabIndex={-1}
                onKeyDown={handleDrawerKeyDown}
                className="nuwa-dialog absolute inset-y-0 left-0 flex w-72 max-w-[85vw] flex-col border-r border-border bg-surface text-text shadow-overlay"
              >
                <div className="flex h-16 items-center gap-2.5 border-b border-border px-5">
                  <ShellBrand />
                  <button
                    ref={drawerCloseRef}
                    type="button"
                    aria-label="关闭导航菜单"
                    onClick={closeDrawer}
                    className="ml-auto flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text"
                  >
                    <X aria-hidden="true" className="size-4" />
                  </button>
                </div>

                <nav aria-label="抽屉主导航" className="flex-1 overflow-y-auto px-3 py-4">
                  <SidebarNavList onNavigate={closeDrawer} />
                </nav>

                <SidebarFooter localMode={localMode} />
              </div>
            </div>,
            document.body,
          )
        : null}

      <nav
        aria-label="移动导航"
        className="fixed inset-x-0 bottom-0 z-30 border-t border-border bg-surface lg:hidden"
      >
        <ul className="mx-auto grid max-w-page grid-cols-4">
          {MOBILE_NAV_ITEMS.map((item) => {
            const Icon = item.icon;
            return (
              <li key={item.label}>
                {item.to ? (
                  <NavLink
                    to={item.to}
                    end={item.end}
                    className={({ isActive }) =>
                      clsx(
                        "flex h-14 flex-col items-center justify-center gap-1 text-caption transition-colors",
                        isActive ? "text-accent" : "text-text-secondary hover:text-text",
                      )
                    }
                  >
                    <Icon aria-hidden="true" className="size-5" />
                    <span>{item.label}</span>
                  </NavLink>
                ) : (
                  <span
                    aria-disabled="true"
                    title={item.soon}
                    className="flex h-14 flex-col items-center justify-center gap-1 text-caption text-text-muted"
                  >
                    <Icon aria-hidden="true" className="size-5" />
                    <span>
                      {item.label}
                      <span className="sr-only">（即将提供）</span>
                    </span>
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      </nav>
    </div>
  );
}
