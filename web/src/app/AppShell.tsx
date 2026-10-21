import { clsx } from "clsx";
import {
  Activity,
  Cpu,
  FolderTree,
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
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, NavLink, Outlet, useLocation } from "react-router-dom";

import { OfflineBanner } from "../components/OfflineBanner";
import { ThemeToggle } from "../components/ThemeToggle";
import { Badge, Button, Skeleton, useToast } from "../components/ui";
import { useOverviewQuery } from "../features/overview/queries";
import { ProjectRail } from "../features/projects/ProjectRail";
import { projectLabel } from "../features/projects/format";
import { useProjectsQuery } from "../features/projects/queries";
import { ApiError } from "../lib/api";
import { useAuth } from "../lib/auth";
import { useModalBehavior } from "../lib/modal";
import { CommandPalette } from "./CommandPalette";

interface NavItem {
  label: string;
  icon: LucideIcon;
  to: string;
  end?: boolean;
}

const NAV_ITEMS: readonly NavItem[] = [
  { label: "总览", icon: LayoutDashboard, to: "/", end: true },
  { label: "问题单", icon: ListTodo, to: "/issues" },
  { label: "运行器", icon: Cpu, to: "/runners" },
  { label: "活动", icon: Activity, to: "/events" },
  { label: "设置", icon: Settings, to: "/settings" },
];

/**
 * Bottom navigation on small screens. The project ledger replaces the
 * activity feed here: a project is the axis the operator thinks in.
 */
const MOBILE_NAV_ITEMS: readonly NavItem[] = [
  { label: "总览", icon: LayoutDashboard, to: "/", end: true },
  { label: "问题单", icon: ListTodo, to: "/issues" },
  { label: "项目", icon: FolderTree, to: "/projects" },
  { label: "运行器", icon: Cpu, to: "/runners" },
];

const DRAWER_ID = "nav-drawer";

interface PageMeta {
  title: string;
  breadcrumb: string | null;
}

function pageMeta(pathname: string): PageMeta {
  if (pathname === "/") {
    return { title: "总览", breadcrumb: null };
  }
  if (pathname === "/projects" || pathname === "/projects/") {
    return { title: "项目台账", breadcrumb: null };
  }
  if (pathname.startsWith("/projects/")) {
    const raw = pathname.slice("/projects/".length);
    if (!raw) {
      return { title: "项目台账", breadcrumb: null };
    }
    let repo = raw;
    try {
      repo = decodeURIComponent(raw);
    } catch {
      // Keep the raw path segment when it is not valid percent-encoding.
    }
    return { title: "项目", breadcrumb: repo };
  }
  if (pathname === "/issues" || pathname === "/issues/") {
    return { title: "问题单", breadcrumb: null };
  }
  if (pathname === "/runners" || pathname === "/runners/") {
    return { title: "运行器", breadcrumb: null };
  }
  if (pathname === "/events" || pathname === "/events/") {
    return { title: "活动", breadcrumb: null };
  }
  if (pathname === "/settings" || pathname === "/settings/") {
    return { title: "设置", breadcrumb: null };
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
 * Polite announcement when the needs-attention count grows: the console polls
 * in the background, so a screen-reader user otherwise never learns that a
 * new escalated/stuck issue arrived while they were on another page.
 */
function AttentionAnnounce({ count }: { count: number }) {
  const [announced, setAnnounced] = useState<number | null>(null);
  const previous = useRef(count);

  useEffect(() => {
    if (count > previous.current) {
      setAnnounced(count);
    }
    previous.current = count;
  }, [count]);

  if (announced === null) {
    return null;
  }
  return (
    <p role="status" className="sr-only">
      需要你处理的事项增加，当前共 {announced} 项
    </p>
  );
}

/** One reading in the status line: a muted label and a mono value. */
function StatusFigure({
  to,
  label,
  value,
  tone,
  title,
}: {
  to?: string;
  label: string;
  value: string | number;
  tone?: "danger";
  title?: string;
}) {
  const body = (
    <>
      <span className="text-caption text-text-muted">{label}</span>
      <span
        className={clsx(
          "ledger__num text-body-sm",
          tone === "danger" ? "text-danger" : "text-text",
        )}
      >
        {value}
      </span>
    </>
  );
  if (!to) {
    return (
      <span className="flex items-baseline gap-1.5" title={title}>
        {body}
      </span>
    );
  }
  return (
    <Link
      to={to}
      title={title}
      className="flex items-baseline gap-1.5 transition-colors hover:text-accent"
    >
      {body}
    </Link>
  );
}

function StatusDivider() {
  return <span aria-hidden="true" className="h-3.5 w-px shrink-0 bg-border" />;
}

/**
 * Global status line (docs/09 §10.1). It reads the numbers of the current
 * scope — the project in view, or every project — as one ledger line
 * (label · number · rule · label · number) instead of a row of identical
 * pills, and it follows the project the operator is looking at.
 */
export function HealthStrip({ scope }: { scope: string | null }) {
  const overview = useOverviewQuery(scope);
  const projects = useProjectsQuery();
  const project = scope
    ? projects.data?.data.find((entry) => entry.repo === scope)
    : undefined;
  const scopeName = scope ? projectLabel(project?.name ?? null, scope) : null;
  const repoQuery = scope ? `&repo=${encodeURIComponent(scope)}` : "";

  let content: ReactNode;
  if (overview.isPending) {
    content = (
      <div role="status" aria-busy="true" className="flex items-center gap-3">
        <span className="sr-only">正在加载系统状态</span>
        <Skeleton className="h-4 w-32" />
        <Skeleton className="h-4 w-24" />
        <Skeleton className="h-4 w-24" />
      </div>
    );
  } else if (!overview.data) {
    content = (
      <div className="flex items-center gap-2">
        <span className="flex items-center gap-1.5 text-body-sm text-danger">
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
    const {
      active_runs: activeRuns,
      attention,
      health,
      queue,
    } = overview.data;
    // WHY: the server's `queue.ready_count` is the GLOBAL claim queue (see
    // crates/nuwa/src/api/overview.rs build_queue: "claim queue is global"), so
    // it ignores the repo filter. Under a project scope the figure links to
    // `?state=new&repo=<scope>` (a project-only list), which let a global
    // backlog masquerade as the project's own — a phantom backlog. Use the
    // project's `queued` (its count of state=new issues, per projects.rs) so the
    // number matches the list it links to; keep the global count for "全部项目".
    const queueCount = scope ? (project?.queued ?? 0) : queue.ready_count;
    content = (
      <>
        {scopeName ? (
          <Link
            to={`/projects/${encodeURIComponent(scope ?? "")}`}
            className="text-body-sm font-medium text-text transition-colors hover:text-accent"
            title={scope ?? undefined}
          >
            {scopeName}
          </Link>
        ) : (
          <span className="text-body-sm font-medium text-text">全部项目</span>
        )}
        {scope ? (
          <Link
            to="/"
            className="text-caption text-text-muted transition-colors hover:text-accent"
          >
            取消限定
          </Link>
        ) : null}
        <StatusDivider />
        <StatusFigure
          to={`/issues?attention=1${repoQuery}`}
          label="需处理"
          value={attention.length}
          tone={attention.length > 0 ? "danger" : undefined}
          title={`需要你处理的事项：${attention.length} 项`}
        />
        <StatusDivider />
        <StatusFigure
          to={`/issues?state=new${repoQuery}`}
          label="队列"
          value={queueCount}
        />
        <StatusDivider />
        <StatusFigure
          to={`/issues?state=triggered,fixing${repoQuery}`}
          label="运行中"
          value={activeRuns.length}
        />
        <StatusDivider />
        <StatusFigure
          to="/runners"
          label="在线"
          value={`${health.runners_online}/${health.runners_total}`}
        />
        <StatusDivider />
        <StatusFigure
          label="今日 PR"
          value={health.prs_today}
          title="今日打开的 PR（按仓库配额统计）"
        />
        {health.standing_down > 0 ? (
          <>
            <StatusDivider />
            <StatusFigure
              to="/runners"
              label="配额待命"
              value={health.standing_down}
              tone="danger"
            />
          </>
        ) : null}
        <StatusDivider />
        <span className="flex items-center gap-1.5 text-body-sm text-text-secondary">
          <StatusDot tone="success" />
          serve 正常
        </span>
        {overview.isError || overview.isRefetchError ? (
          <button
            type="button"
            onClick={() => void overview.refetch()}
            className="flex items-center gap-1.5 text-body-sm text-danger"
          >
            <StatusDot tone="danger" />
            更新失败 · 重试
          </button>
        ) : null}
      </>
    );
  }

  return (
    <section aria-label="全局状态" className="border-b border-border bg-bg">
      <div className="mx-auto flex w-full max-w-page items-center gap-3 overflow-x-auto px-4 py-2 sm:px-6 lg:px-8">
        {overview.data ? <AttentionAnnounce count={overview.data.attention.length} /> : null}
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
  const projects = useProjectsQuery();
  /**
   * The project the operator is looking at: the project page's route
   * parameter, or the `?repo=` scope of a list page. Every global reading
   * follows it.
   */
  const scope = useMemo(() => {
    if (location.pathname.startsWith("/projects/")) {
      const raw = location.pathname.slice("/projects/".length);
      try {
        return decodeURIComponent(raw);
      } catch {
        return raw;
      }
    }
    return new URLSearchParams(location.search).get("repo");
  }, [location.pathname, location.search]);
  const [loggingOut, setLoggingOut] = useState(false);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const menuButtonRef = useRef<HTMLButtonElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);
  const drawerCloseRef = useRef<HTMLButtonElement>(null);
  const paletteReturnFocusRef = useRef<HTMLElement | null>(null);

  const closeDrawer = useCallback(() => setDrawerOpen(false), []);
  const closePalette = useCallback(() => setPaletteOpen(false), []);
  const handleDrawerKeyDown = useModalBehavior({
    active: drawerOpen,
    containerRef: drawerRef,
    initialFocusRef: drawerCloseRef,
    returnFocusRef: menuButtonRef,
    onEscape: closeDrawer,
  });

  /**
   * Opens the palette from `trigger`, or from the focused element for the
   * keyboard shortcut. The drawer closes first: two modals sharing the page
   * lock would leave the palette mounted over a closing drawer. The invoking
   * control (or the focused element, or `<main>` when nothing meaningful is
   * focused) is remembered so closing puts focus back where the user was.
   */
  const openPalette = useCallback((trigger?: HTMLElement | null) => {
    setDrawerOpen(false);
    const active = document.activeElement;
    const candidate =
      trigger ??
      (active instanceof HTMLElement &&
      active !== document.body &&
      active !== document.documentElement &&
      active.isConnected &&
      // The drawer is closing: an element inside it is about to unmount.
      !drawerRef.current?.contains(active)
        ? active
        : null);
    paletteReturnFocusRef.current = candidate?.isConnected
      ? candidate
      : document.getElementById("main-content");
    setPaletteOpen(true);
  }, []);

  // ⌘K / Ctrl+K opens the palette from anywhere (docs/09 §12), including
  // while an input is focused: it is a global command surface, not a page
  // shortcut. A second press toggles it closed.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        if (paletteOpen) {
          setPaletteOpen(false);
        } else {
          openPalette();
        }
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [paletteOpen, openPalette]);

  // The document title follows the active route so tabs, history entries and
  // screen-reader page announcements stay distinguishable. The login page sets
  // its own title.
  useEffect(() => {
    document.title = title === "Nuwa 控制台" ? "女娲 · 补天" : `${title} · 女娲`;
  }, [title]);

  // A route change always dismisses the drawer and the palette (nav links and
  // palette results close themselves; this covers browser back/forward and
  // redirects). The mount pass is skipped: it would otherwise close a drawer
  // or palette that a user opened immediately after the first paint.
  const currentPath = useRef(location.pathname);
  useEffect(() => {
    if (currentPath.current === location.pathname) {
      return;
    }
    currentPath.current = location.pathname;
    setDrawerOpen(false);
    setPaletteOpen(false);
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

        <nav aria-label="主导航" className="flex flex-1 flex-col overflow-hidden px-3 py-4">
          <div className="flex min-h-0 flex-1 flex-col">
            <Link
              to="/projects"
              className="flex min-h-9 items-center gap-1.5 px-2 pb-1 text-caption font-medium text-text-muted transition-colors hover:text-text"
            >
              <FolderTree aria-hidden="true" className="size-3.5" />
              项目
            </Link>
            <div className="min-h-0 flex-1 overflow-y-auto">
              <ProjectRail projects={projects.data?.data ?? []} activeRepo={scope} />
            </div>
          </div>
          <div className="mt-4 border-t border-border pt-3">
            <SidebarNavList />
          </div>
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
                aria-label="搜索问题单"
                aria-haspopup="dialog"
                aria-expanded={paletteOpen}
                onClick={(event) => openPalette(event.currentTarget)}
                className="w-60 justify-start font-normal"
              >
                <Search aria-hidden="true" className="size-3.5 shrink-0" />
                <span className="truncate">搜索问题单…</span>
                <kbd className="ml-auto rounded border border-border px-1 font-mono text-caption">
                  ⌘K
                </kbd>
              </Button>
            </div>

            {/* Touch equivalent of the desktop search field. It is the visible
                return target for the palette on small screens. */}
            <button
              type="button"
              aria-label="搜索问题单"
              aria-haspopup="dialog"
              aria-expanded={paletteOpen}
              onClick={(event) => openPalette(event.currentTarget)}
              className="flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text md:hidden"
            >
              <Search aria-hidden="true" className="size-5" />
            </button>

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

        <HealthStrip scope={scope} />

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

                <nav aria-label="抽屉主导航" className="flex flex-1 flex-col overflow-hidden px-3 py-4">
                  <div className="flex min-h-0 flex-1 flex-col">
                    <Link
                      to="/projects"
                      onClick={closeDrawer}
                      className="flex min-h-9 items-center gap-1.5 px-2 pb-1 text-caption font-medium text-text-muted transition-colors hover:text-text"
                    >
                      <FolderTree aria-hidden="true" className="size-3.5" />
                      项目
                    </Link>
                    <div className="min-h-0 flex-1 overflow-y-auto">
                      <ProjectRail
                        projects={projects.data?.data ?? []}
                        activeRepo={scope}
                      />
                    </div>
                  </div>
                  <div className="mt-4 border-t border-border pt-3">
                    <SidebarNavList onNavigate={closeDrawer} />
                  </div>
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
              </li>
            );
          })}
        </ul>
      </nav>

      <CommandPalette
        open={paletteOpen}
        onClose={closePalette}
        returnFocusRef={paletteReturnFocusRef}
      />
    </div>
  );
}
