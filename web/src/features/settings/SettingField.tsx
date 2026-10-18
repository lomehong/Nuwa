import { useState } from "react";

import { Badge, Button, Input, Select } from "../../components/ui";
import {
  isPlainValue,
  isSecretValue,
  type DiffDto,
  type SettingDto,
  type SettingValueDto,
} from "./types";

interface SettingFieldProps {
  spec: SettingDto;
  value: SettingValueDto | undefined;
  /** Runner-reported runtime value when it differs from the env file. */
  diff?: DiffDto;
  /** Draft text; `undefined` means "show the current value". */
  draft: string | undefined;
  error?: string;
  onChange: (raw: string) => void;
}

/** Read-only text, never an input: the env path is shown but not editable. */
function ReadOnlyField({ spec, current }: { spec: SettingDto; current: string }) {
  return (
    <>
      <p className="text-caption font-medium text-text-secondary">{spec.label}</p>
      <p className="overflow-x-auto rounded-card border border-border bg-raised px-3 py-2 font-mono text-body-sm break-all text-text">
        {current || "—"}
      </p>
    </>
  );
}

/**
 * One schema row (docs/09 §10.8). The control follows `spec.type`; secrets
 * only ever show presence and the last four characters, and the value is
 * rendered by a password input after an explicit "更新" click.
 */
export function SettingField({ spec, value, diff, draft, error, onChange }: SettingFieldProps) {
  // Revealing the password input is per-field state; the parent keys each
  // field by `spec.key`, so a schema refresh cannot leak it across keys.
  const [revealed, setRevealed] = useState(false);

  const secret = isSecretValue(value) ? value : null;
  const current = isPlainValue(value) ? value.value : "";
  const shown = draft ?? current;
  const help = spec.help;
  const meta = (
    <div className="flex flex-wrap items-center gap-1.5">
      {spec.requires_restart ? <Badge variant="warning">需重启</Badge> : null}
      <Badge>作用域 {spec.scope}</Badge>
      {!spec.secret && !spec.read_only && spec.default !== null && spec.default !== "" ? (
        <span className="text-caption text-text-muted">
          默认值 <code>{spec.default}</code>
        </span>
      ) : null}
    </div>
  );
  const diffLine = diff ? (
    <p className="text-caption text-warning">
      文件值 {diff.file} · 运行中 {diff.runtime}
      {diff.source === "default" ? "（默认值）" : ""}
    </p>
  ) : null;

  if (spec.read_only) {
    return (
      <div className="space-y-1.5">
        <ReadOnlyField spec={spec} current={shown} />
        {help ? <p className="text-caption text-text-secondary">{help}</p> : null}
        {meta}
        {diffLine}
      </div>
    );
  }

  if (spec.secret) {
    const secretText = !secret || !secret.set
      ? "未设置"
      : secret.last4
        ? `已设置 · 末 4 位 ${secret.last4}`
        : "已设置 · 末 4 位不可显示";
    return (
      <div className="space-y-1.5">
        <p className="text-caption font-medium text-text-secondary">{spec.label}</p>
        <div className="flex flex-wrap items-center gap-2">
          <p className="font-mono text-body-sm text-text">{secretText}</p>
          <Button
            variant="secondary"
            size="sm"
            aria-expanded={revealed}
            onClick={() => {
              if (revealed) {
                // Hiding the input discards a typed candidate: an unseen draft
                // must never be submitted by a later save.
                onChange("");
              }
              setRevealed(!revealed);
            }}
          >
            {revealed ? "取消" : "更新"}
          </Button>
        </div>
        {revealed ? (
          <Input
            label={`${spec.label} 新值`}
            type="password"
            autoComplete="new-password"
            placeholder="留空保持不变"
            value={draft ?? ""}
            error={error}
            onChange={(event) => onChange(event.target.value)}
          />
        ) : null}
        {help ? <p className="text-caption text-text-secondary">{help}</p> : null}
        {meta}
      </div>
    );
  }

  let control;
  if (spec.type === "enum") {
    control = (
      <Select
        label={spec.label}
        value={shown}
        options={spec.options.map((option) => ({ value: option, label: option }))}
        error={error}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  } else if (spec.type === "int") {
    control = (
      <Input
        label={spec.label}
        type="number"
        inputMode="numeric"
        min={spec.min ?? undefined}
        max={spec.max ?? undefined}
        value={shown}
        error={error}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  } else {
    control = (
      <Input
        label={spec.label}
        type={spec.type === "url" ? "url" : "text"}
        value={shown}
        error={error}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  }

  return (
    <div className="space-y-1.5">
      {control}
      {spec.type === "csv" ? (
        <p className="text-caption text-text-secondary">逗号分隔，逐个 unit 校验。</p>
      ) : null}
      {help ? <p className="text-caption text-text-secondary">{help}</p> : null}
      {meta}
      {diffLine}
    </div>
  );
}
