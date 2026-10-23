/**
 * Wire shapes for `GET /api/v1/events` (`crates/nuwa/src/api/events.rs`).
 *
 * Generated from the Rust DTOs via ts-rs (`web/src/lib/api-types/`, refreshed
 * by the CI `types` job). Do not hand-edit — change the Rust type and let the
 * job regenerate.
 *
 * Raw rows carry `count=1` and equal timestamps; aggregated `reported` rows
 * carry the group size and hour bounds.
 */

export type { EventItem } from "../../lib/api-types/EventItem";
export type { PageMeta } from "../../lib/api-types/PageMeta";
export type { EventListResponse as EventsResponse } from "../../lib/api-types/EventListResponse";
