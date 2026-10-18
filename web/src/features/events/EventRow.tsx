import { Link } from "react-router-dom";

import {
  Badge,
  Card,
  CardContent,
  TableCell,
  TableRow,
} from "../../components/ui";
import { actorLabel } from "../issue-detail/format";
import { issueDetailPath } from "../issues/format";
import {
  eventKindLabel,
  eventKindVariant,
  formatAge,
  formatDateTime,
  isSystemFingerprint,
  isoDateTime,
  shortFingerprint,
} from "../overview/format";
import type { EventItem } from "./types";

/** `__config__` / `__runner__:<id>` / `__system__` have no issue page. */
function systemFingerprintLabel(fingerprint: string): string {
  if (fingerprint === "__config__") {
    return "系统 · 配置";
  }
  if (fingerprint.startsWith("__runner__:")) {
    return "系统 · 运行器";
  }
  return "系统";
}

function EventFingerprint({ fingerprint }: { fingerprint: string }) {
  if (isSystemFingerprint(fingerprint)) {
    return (
      <Badge title={fingerprint} className="shrink-0">
        <span>{systemFingerprintLabel(fingerprint)}</span>
        {/* The label alone drops the id; announce the full fingerprint instead. */}
        <span className="sr-only">（{fingerprint}）</span>
      </Badge>
    );
  }
  return (
    <Link
      to={issueDetailPath(fingerprint)}
      title={fingerprint}
      className="rounded-sm font-mono text-caption text-text-secondary transition-colors hover:text-accent"
    >
      {shortFingerprint(fingerprint)}
    </Link>
  );
}

function KindCell({ event }: { event: EventItem }) {
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <Badge variant={eventKindVariant(event.kind)} title={event.kind}>
        {eventKindLabel(event.kind)}
      </Badge>
      {event.count > 1 ? (
        <span className="font-mono text-caption text-text-muted">×{event.count}</span>
      ) : null}
    </div>
  );
}

function EventDetail({ detail }: { detail: string }) {
  return (
    <p
      className="max-w-prose break-words font-mono text-body-sm text-text-secondary"
      title={detail || undefined}
    >
      {detail || "—"}
    </p>
  );
}

function EventTime({ event, className }: { event: EventItem; className?: string }) {
  return (
    <div className={className}>
      <time
        dateTime={isoDateTime(event.ts_ms)}
        title={formatDateTime(event.ts_ms)}
        className="whitespace-nowrap text-caption text-text-muted"
      >
        {formatAge(event.ts_ms)}
      </time>
      {event.count > 1 ? (
        <p className="mt-0.5 font-mono text-caption text-text-muted">
          {formatDateTime(event.first_ts_ms)} – {formatDateTime(event.last_ts_ms)}
        </p>
      ) : null}
    </div>
  );
}

function Repo({ repo }: { repo: string }) {
  return (
    <span className="break-all font-mono text-caption text-text-secondary" title={repo || undefined}>
      {repo || "—"}
    </span>
  );
}

function Actor({ actor }: { actor: string | null }) {
  return <span className="text-caption text-text-secondary">{actorLabel(actor) ?? "—"}</span>;
}

/** Desktop row (md+): time, kind, issue, detail, repo, actor. */
export function EventRow({ event }: { event: EventItem }) {
  return (
    <TableRow>
      <TableCell className="align-top">
        <EventTime event={event} />
      </TableCell>
      <TableCell className="align-top">
        <KindCell event={event} />
      </TableCell>
      <TableCell className="align-top">
        <EventFingerprint fingerprint={event.fingerprint} />
      </TableCell>
      <TableCell className="max-w-[28rem] align-top">
        <EventDetail detail={event.detail} />
      </TableCell>
      <TableCell className="align-top">
        <Repo repo={event.repo} />
      </TableCell>
      <TableCell className="align-top">
        <Actor actor={event.actor} />
      </TableCell>
    </TableRow>
  );
}

/** Stacked card (<768px); the table is hidden at this breakpoint. */
export function EventCard({ event }: { event: EventItem }) {
  return (
    <li>
      <Card>
        <CardContent className="space-y-2 p-4">
          <div className="flex flex-wrap items-center gap-2">
            <KindCell event={event} />
            <EventTime event={event} className="ml-auto text-right" />
          </div>
          <EventFingerprint fingerprint={event.fingerprint} />
          <EventDetail detail={event.detail} />
          <dl className="grid grid-cols-2 gap-x-3 gap-y-1 text-caption">
            <div className="min-w-0">
              <dt className="text-text-muted">仓库</dt>
              <dd className="mt-0.5">
                <Repo repo={event.repo} />
              </dd>
            </div>
            <div className="min-w-0">
              <dt className="text-text-muted">操作者</dt>
              <dd className="mt-0.5">
                <Actor actor={event.actor} />
              </dd>
            </div>
          </dl>
        </CardContent>
      </Card>
    </li>
  );
}
