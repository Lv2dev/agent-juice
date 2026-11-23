import { t } from "./i18n.js";

export function collectionRetryState(health, viewState) {
  return {
    visible: ["login_required", "app_required", "unavailable", "transient_error", "parse_error", "credentials_error", "source_changed", "rate_limited"].includes(health)
      || ["login_required", "empty"].includes(viewState),
    blocked: health === "rate_limited",
  };
}

export function collectionIssue(tool, health, language) {
  if (tool === "cursor" && health === "credentials_error") {
    return { short: t("collection.cursorCredentialsError", language), detail: t("collection.cursorCredentialsErrorDetail", language) };
  }
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
