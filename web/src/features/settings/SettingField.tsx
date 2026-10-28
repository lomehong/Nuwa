import { clsx } from "clsx";
import { useId, useState } from "react";

import {
  isFullWidthKey,
  secretLabel,
  unitForSpec,
} from "./areas";
import {
  isPlainValue,
  isSecretValue,
  type SettingDto,
  type SettingValueDto,
} from "./types";

interface SettingFieldProps {
  spec: SettingDto;
  value: SettingValueDto | undefined;
  /** Draft text; `undefined` means "show the current value". */
  draft: string | undefined;
  error?: string;
  onChange: (raw: string) => void;
}

function placeholderFor(spec: SettingDto): string {
  if (spec.key === "NUWA_MODEL") {
    return "provider 默认";
  }
  if (spec.key === "NUWA_BASE_URL" || spec.key === "NUWA_PUBLIC_BASE_URL") {
    return "默认";
  }
  return "未设置";
}

/**
 * One schema row in the settings panel. The env key stays visible (operators
 * match it against the file) but the label, control and metadata carry the
 * hierarchy; secrets only ever show presence and the last four characters.
 */
export function SettingField({
  spec,
  value,
  draft,
  error,
  onChange,
}: SettingFieldProps) {
  const id = useId();
  const [revealed, setRevealed] = useState(false);
  const current = isPlainValue(value) ? value.value : "";
  const secret = isSecretValue(value) ? value : null;
  const shown = draft ?? current;
  const changed = draft !== undefined;
  const unit = unitForSpec(spec);
  const helpId = spec.help ? `${id}-help` : undefined;
  const errorId = error ? `${id}-error` : undefined;
  const describedBy = [helpId, errorId].filter(Boolean).join(" ") || undefined;

  const help = spec.help ? (
    <p id={helpId} className="st-help">
      {spec.help}
    </p>
  ) : null;

  const meta = (
    <>
      {changed ? (
        <p className="st-meta">
          <span className="old">{spec.secret ? "已更新" : current || "空"}</span>
          {spec.requires_restart ? <span className="restart">需重启进程确认</span> : null}
        </p>
      ) : null}
    </>
  );

  const errorLine = error ? (
    <p id={errorId} role="alert" className="st-error">
      {error}
    </p>
  ) : null;

  if (spec.read_only) {
    return (
      <div className={clsx("st-field", isFullWidthKey(spec.key) && "is-full")}>
        <span className="st-field-label">{spec.label}</span>
        <code className="st-field-key">{spec.key}</code>
        <div className="st-control">
          <div className="st-readonly">{shown || "—"}</div>
        </div>
        {help}
      </div>
    );
  }

  if (spec.secret) {
    const secretSet = secret?.set ?? false;
    return (
      <div className={clsx("st-field", changed && "is-changed")}>
        <span className="st-field-label">{spec.label}</span>
        <code className="st-field-key">{spec.key}</code>
        <div className="st-control">
          <div className="st-secret">
            <i className={clsx("st-dot", secret?.set ? "ok" : "off")} aria-hidden="true" />
            <span>{secretLabel(value)}</span>
            <button
              type="button"
              aria-expanded={revealed}
              aria-label={
                revealed
                  ? `取消更新 ${spec.label}`
                  : secretSet
                    ? `更新 ${spec.label}`
                    : `设置 ${spec.label}`
              }
              onClick={() => {
                if (revealed) {
                  // Hiding the input discards a typed candidate: an unseen
                  // draft must never be submitted by a later save.
                  onChange("");
                }
                setRevealed(!revealed);
              }}
            >
              {revealed ? "取消" : secretSet ? "更换" : "设置"}
            </button>
          </div>
          {revealed ? (
            <input
              id={id}
              className="st-input mono st-secret-input"
              type="password"
              autoComplete="new-password"
              placeholder="留空保持不变"
              value={draft ?? ""}
              aria-label={`${spec.label} 新值`}
              aria-invalid={error ? true : undefined}
              aria-describedby={describedBy}
              onChange={(event) => onChange(event.target.value)}
            />
          ) : null}
        </div>
        {help}
        {meta}
        {errorLine}
      </div>
    );
  }

  let control;
  if (spec.type === "enum") {
    control = (
      <div className="st-control">
        <select
          id={id}
          className="st-select mono"
          value={shown}
          aria-label={spec.label}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy}
          onChange={(event) => onChange(event.target.value)}
        >
          {spec.options.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      </div>
    );
  } else if (spec.type === "int") {
    control = (
      <div className="st-control">
        <input
          id={id}
          className={clsx("st-input mono", unit && "has-unit")}
          type="number"
          inputMode="numeric"
          min={spec.min ?? undefined}
          max={spec.max ?? undefined}
          value={shown}
          aria-label={spec.label}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy}
          onChange={(event) => onChange(event.target.value)}
        />
        {unit ? <span className="st-unit">{unit}</span> : null}
      </div>
    );
  } else {
    control = (
      <div className="st-control">
        <input
          id={id}
          className={clsx("st-input", spec.type === "url" && "mono")}
          type={spec.type === "url" ? "url" : "text"}
          value={shown}
          placeholder={placeholderFor(spec)}
          spellCheck={false}
          autoComplete="off"
          aria-label={spec.label}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy}
          onChange={(event) => onChange(event.target.value)}
        />
      </div>
    );
  }

  return (
    <div
      className={clsx(
        "st-field",
        changed && "is-changed",
        isFullWidthKey(spec.key) && "is-full",
      )}
    >
      <span className="st-field-label">{spec.label}</span>
      <code className="st-field-key">{spec.key}</code>
      {control}
      {spec.type === "csv" ? (
        <p className="st-help">逗号分隔。</p>
      ) : null}
      {help}
      {meta}
      {errorLine}
    </div>
  );
}
