import { t } from "./i18n.js";

export function collectionIssue(tool, health, language) {
  if (tool !== "claude") return null;
  return issueForHealth(health, language);
}

export function panelCollectionIssue(tool, health, language) {
  if (tool !== "codex") return collectionIssue(tool, health, language);
  if (["unavailable", "credentials_error"].includes(health)) {
    return {
      short: t("collection.codexUnavailable", language),
      detail: t("collection.codexUnavailableDetail", language),
    };
  }
  return issueForHealth(health, language);
}

function issueForHealth(health, language) {
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
