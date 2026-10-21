/**
 * 架构图 page: frames the self-contained archify artifact served at
 * `/architecture.html` (built from `web/public/`, embedded into the binary by
 * `nuwa serve`). The artifact owns its pan/zoom/search/theme viewer, so this
 * page is a plain frame — the CSP grants inline content and same-origin
 * framing to that one file only (serve.rs `frame_policy_for`).
 */
export function ArchitecturePage() {
  return (
    <section aria-label="架构图" className="space-y-3">
      <iframe
        src="/architecture.html"
        title="Nuwa 架构图"
        className="h-[calc(100vh-16rem)] min-h-[560px] w-full rounded-card border border-border bg-surface"
      />
      <p className="text-caption text-text-secondary">
        采集 → 问题单 → 修复 → 交付的组件与数据流总览；图内支持缩放、路径追踪与明暗切换（视图右上角）。
      </p>
    </section>
  );
}
