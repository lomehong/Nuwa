import { FileSearch } from "lucide-react";
import { useParams } from "react-router-dom";

import { EmptyState } from "../../components/ui";

/**
 * Placeholder route for Task 6. The AppShell owns the page's h1; page content
 * starts at h2 so the landmark structure stays valid.
 */
export function IssueDetailPage() {
  const { fp } = useParams<{ fp: string }>();
  const fingerprint = fp ?? "";

  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-title font-semibold text-text">问题单详情</h2>
        <p className="mt-1 font-mono text-body-sm break-all text-text-secondary">
          {fingerprint || "未知指纹"}
        </p>
      </header>
      <EmptyState
        icon={FileSearch}
        title="问题单详情即将上线"
        description="时间线、证据面板、运行摘要与转录查看器将在 Task 6 接入 /api/v1/issues/{fp} 与转录端点。"
      />
    </div>
  );
}
