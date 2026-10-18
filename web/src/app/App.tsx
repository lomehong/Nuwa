import { Bell, Gem, Trash2 } from "lucide-react";
import { useState } from "react";

import { ThemeToggle } from "../components/ThemeToggle";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  ConfirmDialog,
  EmptyState,
  Input,
  Select,
  SeverityStone,
  Skeleton,
  StateBadge,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
  ToastProvider,
  useToast,
  type BadgeVariant,
  type IssueState,
  type Severity,
} from "../components/ui";

const SEVERITIES: readonly Severity[] = ["critical", "high", "medium", "low", "unknown"];

const STATES: readonly IssueState[] = [
  "new",
  "triggered",
  "fixing",
  "pr_open",
  "merged",
  "rejected",
  "escalated",
];

const BADGE_VARIANTS: readonly BadgeVariant[] = [
  "neutral",
  "accent",
  "info",
  "success",
  "warning",
  "danger",
];

const SEVERITY_OPTIONS = SEVERITIES.map((severity) => ({
  value: severity,
  label: severity,
}));

const SAMPLE_ISSUES: ReadonlyArray<{
  id: string;
  title: string;
  severity: Severity;
  state: IssueState;
  updated: string;
}> = [
  {
    id: "NUWA-7f3a",
    title: "worker 重试后丢失心跳，run 卡在 repairing",
    severity: "critical",
    state: "fixing",
    updated: "2026-10-18 14:32",
  },
  {
    id: "NUWA-2c91",
    title: "验证门把 flaky 测试误判为回归",
    severity: "high",
    state: "pr_open",
    updated: "2026-10-18 13:07",
  },
  {
    id: "NUWA-8b04",
    title: "ingest 对重复指纹的冷却提示缺失",
    severity: "medium",
    state: "new",
    updated: "2026-10-18 11:45",
  },
  {
    id: "NUWA-51de",
    title: "依赖升级引入的噪声告警",
    severity: "low",
    state: "rejected",
    updated: "2026-10-17 22:18",
  },
];

function Showcase() {
  const { toast } = useToast();
  const [confirmOpen, setConfirmOpen] = useState(false);

  return (
    <div className="min-h-screen bg-bg text-text">
      <div className="mx-auto flex w-full max-w-page flex-col gap-6 px-4 py-8 sm:px-6 lg:px-8">
        <header className="flex flex-wrap items-start justify-between gap-4 border-b border-border pb-6">
          <div>
            <p className="flex items-center gap-2 text-caption font-semibold tracking-[0.18em] text-accent uppercase">
              <Gem aria-hidden="true" className="size-3.5" />
              Nuwa Design System
            </p>
            <h1 className="mt-2 text-title font-semibold text-text">Nuwa 控制台</h1>
            <p className="mt-1 max-w-2xl text-body-sm text-text-secondary">
              设计令牌与基础组件预览。深色 / 浅色主题、五色石严重度、状态徽章与键盘可用性均在此验证；
              Task 3 会用它构建真实的登录与外壳。
            </p>
          </div>
          <ThemeToggle />
        </header>

        <main className="grid gap-4 md:grid-cols-2">
          <Card>
            <CardHeader>
              <div>
                <CardTitle>严重度 · 五色石</CardTitle>
                <CardDescription>图标、文本与颜色三通道表达，不允许只靠颜色。</CardDescription>
              </div>
            </CardHeader>
            <CardContent>
              <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
                {SEVERITIES.map((severity) => (
                  <SeverityStone key={severity} severity={severity} />
                ))}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>问题单状态</CardTitle>
                <CardDescription>状态点与文字同时出现，类名由状态动态映射。</CardDescription>
              </div>
            </CardHeader>
            <CardContent>
              <div className="flex flex-wrap items-center gap-2">
                {STATES.map((state) => (
                  <StateBadge key={state} state={state} />
                ))}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>Badge</CardTitle>
                <CardDescription>通用计数与次级元数据，语义色只用于状态。</CardDescription>
              </div>
            </CardHeader>
            <CardContent>
              <div className="flex flex-wrap items-center gap-2">
                {BADGE_VARIANTS.map((variant) => (
                  <Badge key={variant} variant={variant}>
                    {variant}
                  </Badge>
                ))}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>按钮</CardTitle>
                <CardDescription>四种变体、禁用态与进行中态；触控目标不小于 44px。</CardDescription>
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex flex-wrap items-center gap-2">
                <Button>主要操作</Button>
                <Button variant="secondary">次要操作</Button>
                <Button variant="ghost">幽灵按钮</Button>
                <Button variant="danger">危险操作</Button>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <Button disabled>已禁用</Button>
                <Button pending>处理中</Button>
                <Button size="sm" variant="secondary">
                  小尺寸
                </Button>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>表单</CardTitle>
                <CardDescription>label 始终关联输入；错误通过 role=alert 播报。</CardDescription>
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              <Input
                label="搜索问题"
                placeholder="标题、指纹或 PR 编号"
                hint="支持键盘操作，输入即过滤。"
              />
              <Select
                label="严重度筛选"
                defaultValue="critical"
                options={SEVERITY_OPTIONS}
                hint="只会显示所选严重度的问题单。"
              />
              <Input label="仓库" defaultValue="" error="仓库不能为空" />
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>表格</CardTitle>
                <CardDescription>语义化 table，可横向滚动并从键盘聚焦。</CardDescription>
              </div>
            </CardHeader>
            <CardContent>
              <Table caption="最近问题单（示例数据）" captionVisible>
                <TableHead>
                  <TableRow className="hover:bg-transparent">
                    <TableHeaderCell>问题单</TableHeaderCell>
                    <TableHeaderCell>严重度</TableHeaderCell>
                    <TableHeaderCell>状态</TableHeaderCell>
                    <TableHeaderCell>更新时间</TableHeaderCell>
                  </TableRow>
                </TableHead>
                <TableBody>
                  {SAMPLE_ISSUES.map((issue) => (
                    <TableRow key={issue.id}>
                      <TableCell>
                        <span className="block font-mono text-caption text-text-secondary">{issue.id}</span>
                        <span className="block text-body">{issue.title}</span>
                      </TableCell>
                      <TableCell>
                        <SeverityStone severity={issue.severity} />
                      </TableCell>
                      <TableCell>
                        <StateBadge state={issue.state} />
                      </TableCell>
                      <TableCell className="font-mono text-caption text-text-secondary tabular-nums">
                        {issue.updated}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>空态与骨架</CardTitle>
                <CardDescription>加载中与无数据都有明确文本，不依赖动画或颜色。</CardDescription>
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              <EmptyState
                title="还没有问题单"
                description="接入 qingniao-go SDK 或等待运行时信号进入后，这里会显示问题单。"
                action={
                  <Button size="sm" variant="secondary">
                    查看接入手册
                  </Button>
                }
              />
              <div role="status" aria-label="加载示例" aria-busy="true" className="space-y-2">
                <span className="sr-only">正在加载示例数据</span>
                <Skeleton className="h-3 w-2/3" />
                <Skeleton className="h-3 w-1/2" />
                <Skeleton className="h-16 w-full" />
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <div>
                <CardTitle>浮层反馈</CardTitle>
                <CardDescription>Toast 使用 aria-live 礼貌播报；确认框带焦点陷阱与 Esc 关闭。</CardDescription>
              </div>
            </CardHeader>
            <CardContent className="flex flex-wrap gap-2">
              <Button
                variant="secondary"
                onClick={() =>
                  toast({
                    title: "问题单已加入队列",
                    description: "runner 将在下一轮 work 中领取。",
                    variant: "success",
                  })
                }
              >
                <Bell aria-hidden="true" className="size-4" />
                显示 Toast
              </Button>
              <Button variant="danger" onClick={() => setConfirmOpen(true)}>
                <Trash2 aria-hidden="true" className="size-4" />
                打开确认框
              </Button>
            </CardContent>
          </Card>
        </main>

        <footer className="border-t border-border pt-4 text-caption text-text-secondary">
          颜色来自 docs/09 §11；所有示例均无内联样式，可在严格 CSP 下运行。
        </footer>
      </div>

      <ConfirmDialog
        open={confirmOpen}
        title="删除演示数据？"
        description="此操作仅影响示例展示，不会调用后端。确认后会显示一条成功 Toast。"
        confirmLabel="删除"
        danger
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => {
          setConfirmOpen(false);
          toast({
            title: "演示数据已删除",
            description: "这是一次本地演示，没有发送请求。",
            variant: "success",
          });
        }}
      />
    </div>
  );
}

export default function App() {
  return (
    <ToastProvider>
      <Showcase />
    </ToastProvider>
  );
}
