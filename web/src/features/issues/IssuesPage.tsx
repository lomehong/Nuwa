import { ListTodo } from "lucide-react";

import { EmptyState } from "../../components/ui";

/**
 * Placeholder route for Task 5. The AppShell owns the page's h1; page content
 * starts at h2 so the landmark structure stays valid.
 */
export function IssuesPage() {
  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-title font-semibold text-text">问题单</h2>
        <p className="mt-1 text-body-sm text-text-secondary">
          按状态、严重度、仓库与语言筛选，查看每个指纹的修复进度。
        </p>
      </header>
      <EmptyState
        icon={ListTodo}
        title="问题单列表即将上线"
        description="服务端分页的问题单表格、筛选栏与移动端卡片将在 Task 5 接入 /api/v1/issues。"
      />
    </div>
  );
}
