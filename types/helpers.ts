import type { LocalizedMessage, MetricValue } from "./report";

/** Type guard: returns `true` when a `MetricValue` is a `LocalizedMessage`. */
export function isLocalizedMessage(v: MetricValue): v is LocalizedMessage {
  return typeof v === "object" && v !== null && !Array.isArray(v) && v.type === "i18n";
}
