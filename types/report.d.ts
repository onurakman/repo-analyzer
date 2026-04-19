/**
 * TypeScript types for repo-analyzer JSON report output.
 *
 * Generated from the Rust types in `src/types.rs` and the JSON writer
 * in `src/output/json.rs`. Keep in sync when the Rust schema changes.
 */

// ---------------------------------------------------------------------------
// Top-level
// ---------------------------------------------------------------------------

/** Root JSON object returned by `--format json`. */
export interface RepoAnalyzerReport {
  generated_at: string; // ISO 8601: "2025-04-19T11:30:00Z"
  reports: Record<string, Report>;
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/**
 * One metric collector's output. The report either has flat `entries`
 * or grouped `entry_groups` — never both at the same time.
 */
export type Report = FlatReport | GroupedReport;

interface ReportBase {
  name: string;
  display_name: LocalizedMessage;
  description: LocalizedMessage;
  columns: Column[];
  total_entries: number;
  shown_entries: number;
}

export interface FlatReport extends ReportBase {
  entries: MetricEntry[];
  entry_groups?: never;
}

export interface GroupedReport extends ReportBase {
  entries?: never;
  entry_groups: EntryGroup[];
}

// ---------------------------------------------------------------------------
// Localization
// ---------------------------------------------------------------------------

export type Severity = "info" | "warning" | "error" | "critical";

/**
 * A structured, localizable message. Consumers look up `code` in a locale
 * file (e.g. `locales/en.json`), substitute `params`, and apply `severity`
 * as a UI badge.
 *
 * Identified in JSON by the constant `"type": "i18n"` field — any value
 * carrying this field is translatable.
 */
export interface LocalizedMessage {
  type: "i18n";
  code: string;
  severity?: Severity;
  params?: Record<string, string | number | boolean>;
}

// ---------------------------------------------------------------------------
// Columns
// ---------------------------------------------------------------------------

/** Describes one table column. `value` is the key into `MetricEntry`. */
export interface Column {
  value: string;
  label: LocalizedMessage;
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/**
 * Polymorphic metric value. Because Rust serializes `MetricValue` as
 * `#[serde(untagged)]`, the JSON type is a bare union:
 *
 * - `number`  — Count (u64), SignedCount (i64), Float (f64)
 * - `string`  — Text or Date (YYYY-MM-DD)
 * - `LocalizedMessage` — translatable message (has `type: "i18n"`)
 * - `MetricValue[]` — nested list
 */
export type MetricValue =
  | number
  | string
  | LocalizedMessage
  | MetricValue[];

/**
 * One row in a report table. `name` is the row key (file path, author
 * name, etc.). Remaining keys correspond to `Column.value` identifiers.
 */
export interface MetricEntry {
  name: string;
  [column: string]: MetricValue;
}

// ---------------------------------------------------------------------------
// Entry groups
// ---------------------------------------------------------------------------

/** A named bucket of entries (e.g. "pillars", "actions" in health). */
export interface EntryGroup {
  name: string;
  label: string;
  entries: MetricEntry[];
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Type guard: returns `true` when a `MetricValue` is a `LocalizedMessage`. */
export declare function isLocalizedMessage(v: MetricValue): v is LocalizedMessage;
