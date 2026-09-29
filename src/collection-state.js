import { t } from "./i18n.js";

export function collectionIssue(tool, health, language) {
  if (tool !== "claude") return null;
  const key = {
    rate_limited: "rateLimited",
    parse_error: "parseError",
    credentials_error: "credentialsError",
    source_changed: "sourceChanged",
    transient_error: "transportError",
    unavailable: "sourceUnavailable",
  }[health];
  return key ? { short: t(`collection.${key}`, language), detail: t(`collection.${key}Detail`, language) } : null;
}
