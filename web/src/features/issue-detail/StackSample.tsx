import { Card, CardContent } from "../../components/ui";
import { CopyButton } from "./CopyButton";
import { IssueSection } from "./IssueSection";

interface StackSampleProps {
  text: string;
}

/**
 * Stack sample (docs/09 §10.5): rendered as a React text node inside `<pre>`
 * (never `dangerouslySetInnerHTML`), preserving newlines and offering a copy
 * action. The box scrolls instead of stretching the page.
 */
export function StackSample({ text }: StackSampleProps) {
  const sample = text ?? "";

  return (
    <IssueSection
      id="issue-stack"
      title="堆栈样本"
      description="上报时保留的堆栈文本，原样展示。"
      aside={
        sample ? (
          <CopyButton
            value={sample}
            label="复制堆栈样本"
            toastTitle="已复制堆栈样本"
            text="复制"
          />
        ) : null
      }
    >
      <Card>
        <CardContent className="p-4">
          {sample ? (
            <pre className="max-h-96 overflow-auto rounded-card border border-border bg-raised/50 p-3 font-mono text-caption break-words whitespace-pre-wrap text-text-secondary">
              {sample}
            </pre>
          ) : (
            <p className="text-body-sm text-text-secondary">无堆栈样本。</p>
          )}
        </CardContent>
      </Card>
    </IssueSection>
  );
}
