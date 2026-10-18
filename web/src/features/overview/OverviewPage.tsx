import { LayoutDashboard } from "lucide-react";

import { EmptyState } from "../../components/ui";

/**
 * Placeholder route for Task 4. The AppShell owns the page's h1; page content
 * starts at h2 so the landmark structure stays valid.
 */
export function OverviewPage() {
  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-title font-semibold text-text">总览</h2>
        <p className="mt-1 text-body-sm text-text-secondary">
          需要你处理的事项、正在发生的运行与系统健康将在这里汇总。
        </p>
      </header>
      <EmptyState
        icon={LayoutDashboard}
        title="总览即将上线"
        description="总览页（需处理队列、活动运行、趋势与漏斗）将在 Task 4 接入 /api/v1/overview 与 /api/v1/metrics/trends。"
      />
    </div>
  );
}
