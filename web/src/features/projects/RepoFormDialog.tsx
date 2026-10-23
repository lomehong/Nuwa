import { clsx } from "clsx";
import { X } from "lucide-react";
import { useId, useRef, useState, type FormEvent } from "react";
import { createPortal } from "react-dom";

import { Button, Input, useToast } from "../../components/ui";
import { ApiError } from "../../lib/api";
import { describeError } from "../../lib/errors";
import { useModalBehavior } from "../../lib/modal";
import { useCreateRepoMutation, useUpdateRepoMutation } from "./registry";
import type { RepoDto } from "./registry";

export type RepoFormMode = "create" | "edit";

/** Values a runner-only group can prefill: its identity and display name. */
export interface RepoFormPrefill {
  repo: string;
  name: string;
}

export interface RepoFormDialogProps {
  open: boolean;
  mode: RepoFormMode;
  /** Overrides the heading when the host page has its own wording. */
  title?: string;
  /** Overrides the submit button label when the host page has its own wording. */
  submitLabel?: string;
  /** Edit target; its identity is path-owned and never editable. */
  target?: RepoDto | null;
  /** Create prefill, e.g. from a runner-only group. */
  prefill?: RepoFormPrefill | null;
  /**
   * Lock the identity even in create mode — the project already exists
   * (it reported signals), so its id must not be typed over.
   */
  lockIdentity?: boolean;
  onClose: () => void;
}

/** Mirrors `crates/nuwa/src/api/repos.rs` so the server is not the first check. */
const MAX_NAME_LEN = 128;
const MAX_REPO_LEN = 256;
const MAX_URL_LEN = 512;
const MAX_DESCRIPTION_LEN = 1000;

const REPO_PATTERN = /^[A-Za-z0-9._:/-]+$/;
const REPO_URL_HINT =
  "HTTP/HTTPS、SSH、git 或无协议仓库地址，例如 https://example.com/app 或 git@example.com:app.git";

interface RepoFormValues {
  repo: string;
  name: string;
  url: string;
  description: string;
}

/**
 * The field values a dialog seeds from, derived once per render from the
 * current target/prefill. `repo` is the identity the form is bound to.
 */
function seedValues(
  target: RepoDto | null | undefined,
  prefill: RepoFormPrefill | null | undefined,
): RepoFormValues {
  return {
    repo: target?.repo ?? prefill?.repo ?? "",
    name: target?.name ?? prefill?.name ?? "",
    url: target?.url ?? "",
    description: target?.description ?? "",
  };
}

function charCount(value: string): number {
  return Array.from(value).length;
}

function isRepoUrl(url: string): boolean {
  // Match valid_repo_url: keep scheme-less paths and scp-style addresses
  // editable, rather than imposing a stricter URL parser on existing data.
  const colon = url.indexOf(":");
  const head = url.slice(0, colon).toLowerCase();
  if (colon < 0 || head.includes("/")) {
    return true;
  }
  return head.includes("@") || ["http", "https", "ssh", "git"].includes(head);
}

/** Mirrors Rust's `char::is_control` without a control-character regex. */
function hasControlChar(value: string): boolean {
  for (const char of value) {
    const code = char.codePointAt(0) ?? 0;
    if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) {
      return true;
    }
  }
  return false;
}

/** Client-side mirror of the API's field validation; keys match `fields`. */
export function validateRepoForm(
  values: RepoFormValues,
  identityLocked: boolean,
): Record<string, string> {
  const errors: Record<string, string> = {};

  if (!identityLocked) {
    if (values.repo === "") {
      errors.repo = "请填写仓库标识";
    } else if (charCount(values.repo) > MAX_REPO_LEN) {
      errors.repo = `仓库标识最多 ${MAX_REPO_LEN} 个字符`;
    } else if (!REPO_PATTERN.test(values.repo)) {
      errors.repo = "只能包含 ASCII 字母、数字和 - . _ : /";
    }
  }

  if (values.name === "") {
    errors.name = "请填写名称";
  } else if (charCount(values.name) > MAX_NAME_LEN) {
    errors.name = `名称最多 ${MAX_NAME_LEN} 个字符`;
  } else if (hasControlChar(values.name)) {
    errors.name = "名称不能包含控制字符";
  }

  if (values.url === "") {
    errors.url = "请填写仓库地址";
  } else if (charCount(values.url) > MAX_URL_LEN) {
    errors.url = `仓库地址最多 ${MAX_URL_LEN} 个字符`;
  } else if (/\p{White_Space}/u.test(values.url) || hasControlChar(values.url)) {
    errors.url = "仓库地址不能包含空白或控制字符";
  } else if (!isRepoUrl(values.url)) {
    errors.url = `请填写 ${REPO_URL_HINT}`;
  }

  if (charCount(values.description) > MAX_DESCRIPTION_LEN) {
    errors.description = `描述最多 ${MAX_DESCRIPTION_LEN} 个字符`;
  }

  return errors;
}

/**
 * Create/edit dialog for the repository registry. Only the display identity is
 * editable here: repository content (`failing_check`, `forbidden_paths`,
 * `notes`) lives in the target repo and never enters this form.
 */
export function RepoFormDialog({
  open,
  mode,
  title,
  submitLabel,
  target = null,
  prefill = null,
  lockIdentity = false,
  onClose,
}: RepoFormDialogProps) {
  const create = useCreateRepoMutation();
  const update = useUpdateRepoMutation();
  const { toast } = useToast();
  const editing = mode === "edit";
  const identityLocked = editing || lockIdentity;
  const pending = create.isPending || update.isPending;

  const seed = seedValues(target, prefill);
  const [repo, setRepo] = useState(seed.repo);
  const [name, setName] = useState(seed.name);
  const [url, setUrl] = useState(seed.url);
  const [description, setDescription] = useState(seed.description);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [wasOpen, setWasOpen] = useState(open);
  // The identity the visible fields were last seeded from. Used to detect that
  // an already-open dialog has been re-pointed at a different project.
  const [seededIdentity, setSeededIdentity] = useState(seed.repo);

  const dialogRef = useRef<HTMLDivElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const descriptionId = useId();
  const titleId = useId();

  // Every open starts fresh from its target/prefill (same pattern as the
  // command palette): reset during render so a reopened dialog never flashes
  // the previous entry's values.
  //
  // WHY the identity branch: this dialog is reused across the same route, so a
  // history navigation can swap `target` from project A to project B while
  // `open` stays true. Resetting only on the open transition would leave A's
  // name/url/description on screen but submit them against B's identity,
  // writing A onto B. Re-seeding whenever `target?.repo`/`prefill?.repo` changes
  // keeps the fields, the shown identity and the submit target bound to one
  // project.
  const seedFields = () => {
    setSeededIdentity(seed.repo);
    setRepo(seed.repo);
    setName(seed.name);
    setUrl(seed.url);
    setDescription(seed.description);
    setErrors({});
  };
  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) {
      seedFields();
    }
  } else if (open && seed.repo !== seededIdentity) {
    seedFields();
  }


  function close() {
    if (pending) {
      return;
    }
    onClose();
  }

  const handleKeyDown = useModalBehavior({
    active: open,
    containerRef: dialogRef,
    initialFocusRef: nameRef,
    onEscape: close,
  });

  if (!open) {
    return null;
  }

  function clearError(key: keyof RepoFormValues) {
    setErrors((current) => {
      if (!(key in current)) {
        return current;
      }
      const next = { ...current };
      delete next[key];
      return next;
    });
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (pending) {
      return;
    }
    const values: RepoFormValues = {
      repo: repo.trim(),
      name: name.trim(),
      url: url.trim(),
      description: description.trim(),
    };
    const nextErrors = validateRepoForm(values, identityLocked);
    if (Object.keys(nextErrors).length > 0) {
      setErrors(nextErrors);
      return;
    }
    // Capture the submit target from the seeded `repo` state, NOT from the
    // `target` prop: the prop can be re-pointed at another project by a render
    // between opening and submitting, while `repo` is the identity this form
    // instance was seeded with and stays fixed for this one submission.
    const identity = identityLocked ? repo : values.repo;
    const onSuccess = () => {
      toast({
        title: editing ? "仓库已更新" : "仓库已添加",
        description: `${values.name}（${identity}）`,
        variant: "success",
      });
      onClose();
    };
    const onError = (error: unknown) => {
      if (error instanceof ApiError && error.fields) {
        setErrors(error.fields);
      }
      toast({
        title: editing ? "保存失败" : "添加失败",
        description: describeError(error),
        variant: "danger",
      });
    };
    if (editing) {
      update.mutate(
        {
          repo: identity,
          name: values.name,
          url: values.url,
          description: values.description,
        },
        { onSuccess, onError },
      );
    } else {
      create.mutate(
        {
          repo: values.repo,
          name: values.name,
          url: values.url,
          description: values.description,
        },
        { onSuccess, onError },
      );
    }
  }

  return createPortal(
    <div
      role="presentation"
      className="nuwa-backdrop fixed inset-0 z-50 flex items-end justify-center bg-scrim p-4 sm:items-center"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          close();
        }
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={handleKeyDown}
        className="nuwa-dialog max-h-[calc(100vh-2rem)] w-full max-w-lg overflow-y-auto rounded-modal border border-border-strong bg-surface p-5 text-text shadow-overlay"
      >
        <div className="flex items-start justify-between gap-4">
          <h2 id={titleId} className="text-heading-sm font-semibold">
            {title ?? (editing ? "编辑仓库" : "添加仓库")}
          </h2>
          <button
            type="button"
            aria-label="关闭对话框"
            onClick={close}
            disabled={pending}
            className="-mr-2 -mt-2 flex size-11 shrink-0 items-center justify-center rounded-card text-text-secondary transition-colors duration-150 ease-out hover:bg-raised hover:text-text disabled:pointer-events-none disabled:opacity-50"
          >
            <X aria-hidden="true" className="size-4" />
          </button>
        </div>
        <p className="mt-1 text-body-sm text-text-secondary">
          登记用于把问题单与 runner 匹配起来；仓库内容（验证命令、禁改路径）仍由目标仓库的
          nuwa.repo.json 决定。
        </p>

        <form className="mt-4 space-y-4" noValidate onSubmit={handleSubmit}>
          <Input
            ref={nameRef}
            label="名称"
            value={name}
            maxLength={MAX_NAME_LEN}
            disabled={pending}
            error={errors.name}
            onChange={(event) => {
              setName(event.target.value);
              clearError("name");
            }}
          />

          {identityLocked ? (
            <div className="flex w-full flex-col gap-1.5">
              <p className="text-caption font-medium text-text-secondary">仓库标识</p>
              <p className="rounded-card border border-border bg-raised px-3 py-2 font-mono text-body-sm break-all text-text">
                {repo}
              </p>
              <p className="text-caption text-text-secondary">
                用于匹配问题单与 runner，例如 example.com/app；登记后不可修改。
              </p>
            </div>
          ) : (
            <Input
              label="仓库标识"
              value={repo}
              maxLength={MAX_REPO_LEN}
              disabled={pending}
              hint="用于匹配问题单与 runner，例如 example.com/app"
              error={errors.repo}
              onChange={(event) => {
                setRepo(event.target.value);
                clearError("repo");
              }}
            />
          )}

          <Input
            label="仓库地址"
            value={url}
            maxLength={MAX_URL_LEN}
            disabled={pending}
            hint={REPO_URL_HINT}
            error={errors.url}
            onChange={(event) => {
              setUrl(event.target.value);
              clearError("url");
            }}
          />

          <div className="flex w-full flex-col gap-1.5">
            <label
              htmlFor={descriptionId}
              className="text-caption font-medium text-text-secondary"
            >
              描述（可选）
            </label>
            <textarea
              id={descriptionId}
              rows={3}
              maxLength={MAX_DESCRIPTION_LEN}
              value={description}
              disabled={pending}
              aria-invalid={errors.description ? true : undefined}
              aria-describedby={errors.description ? `${descriptionId}-error` : undefined}
              onChange={(event) => {
                setDescription(event.target.value);
                clearError("description");
              }}
              className={clsx(
                "min-h-11 w-full rounded-card border bg-surface px-3 py-2 text-body text-text",
                "placeholder:text-text-secondary transition-colors duration-150 ease-out",
                "disabled:cursor-not-allowed disabled:opacity-60",
                errors.description
                  ? "border-danger"
                  : "border-border hover:border-border-strong focus-visible:border-accent",
              )}
            />
            {errors.description ? (
              <p
                id={`${descriptionId}-error`}
                role="alert"
                className="text-caption font-medium text-danger"
              >
                {errors.description}
              </p>
            ) : null}
          </div>

          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="secondary" disabled={pending} onClick={close}>
              取消
            </Button>
            <Button type="submit" pending={pending}>
              {submitLabel ?? (editing ? "保存" : "添加")}
            </Button>
          </div>
        </form>
      </div>
    </div>,
    document.body,
  );
}
