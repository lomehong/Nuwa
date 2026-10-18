import { Copy } from "lucide-react";

import { Button, useToast } from "../../components/ui";

interface CopyButtonProps {
  value: string;
  /** Accessible name, for example "复制提交 SHA：abc123". */
  label: string;
  toastTitle?: string;
  /** Optional visible text; icon-only buttons keep the label for AT only. */
  text?: string;
  className?: string;
}

/**
 * Clipboard action used for fingerprints, commits, branches and stack samples.
 * The strict CSP is irrelevant here (no inline style/script); failure falls
 * back to a toast that tells the operator to copy manually.
 */
export function CopyButton({
  value,
  label,
  toastTitle = "已复制",
  text,
  className,
}: CopyButtonProps) {
  const { toast } = useToast();

  async function copy() {
    if (!value) {
      return;
    }
    if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
      toast({
        title: "复制失败",
        description: "当前浏览器不支持剪贴板访问，请手动选择文本复制。",
        variant: "danger",
      });
      return;
    }
    try {
      await navigator.clipboard.writeText(value);
      toast({ title: toastTitle, description: value, variant: "success" });
    } catch {
      toast({
        title: "复制失败",
        description: "无法访问剪贴板，请手动选择文本复制。",
        variant: "danger",
      });
    }
  }

  return (
    <Button
      variant="ghost"
      size="sm"
      aria-label={label}
      title={label}
      className={className}
      onClick={() => void copy()}
    >
      <Copy aria-hidden="true" className="size-3.5" />
      {text ? <span>{text}</span> : null}
    </Button>
  );
}
