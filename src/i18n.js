const LANGUAGES = new Set(["system", "ko", "en"]);

const COPY = {
  ko: {
    "aria.windowControls": "창 제어",
    "window.minimize": "최소화",
    "window.maximize": "최대화",
    "window.close": "닫기",
    "scope.local": "로컬",
    "limit.fiveHour": "5h",
    "limit.weekly": "주간",
    "reset.prefix": "리셋",
    "reset.past": "리셋 지남",
    "context.label": "컨텍스트",
    "state.stale": "오래됨",
    "meta.approx": "근사치",
    "empty.claude": "Juice 실행 후 Claude를 한 번 사용하면 표시됩니다",
    "empty.codex": "Codex를 이 PC에서 한 번 사용하면 표시됩니다",
    "settings.title": "설정",
    "section.appearance": "외형",
    "section.limits": "잔여량·수집",
    "section.taskbar": "작업표시줄",
    "section.indicator": "원·바 세부",
    "section.system": "시스템",
    "section.lab": "실험실",
    "field.theme": "테마",
    "field.language": "언어",
    "field.font": "폰트",
    "field.palette": "팔레트",
    "field.safe": "안전",
    "field.warning": "경고",
    "field.danger": "위험",
    "field.remainingWarning": "잔여량 경고",
    "field.remainingDanger": "잔여량 위험",
    "field.pollInterval": "수집주기",
    "field.staleAfter": "오래됨",
    "help.remainingThresholds": "작업표시줄은 남은 사용량을 표시하므로, 이 값 이하가 되면 경고/위험 색을 씁니다.",
    "help.staleAfter": "마지막 기록 후 오래됨 표시까지.",
    "unit.seconds": "초",
    "field.barMode": "바 모드",
    "field.limitOrder": "한도 순서",
    "field.indicatorStyle": "표시",
    "field.fullscreenHide": "전체화면 앱에서 숨김",
    "field.maximizedHide": "전체창 숨김",
    "field.showClaude": "Claude 표시",
    "field.showCodex": "Codex 표시",
    "field.ring": "링",
    "field.ringNumbers": "링 숫자",
    "field.numberOutline": "숫자 윤곽",
    "field.numberOutlineWidth": "숫자 윤곽 두께",
    "field.ringSize": "원 크기",
    "field.ringThickness": "링 두께",
    "field.ringGap": "링 간격",
    "field.centerGap": "중앙 공간",
    "field.ringNumberSize": "원 숫자 크기",
    "field.ringNumberWeight": "원 숫자 굵기",
    "field.infoTextSize": "정보 글자 크기",
    "field.infoTextWeight": "정보 글자 굵기",
    "field.autostart": "자동시작",
    "field.claudeUsageAutoRefresh": "Claude /usage 자동 새로고침",
    "help.claudeUsageAutoRefresh": "수동 새로고침에서만 쓰던 Claude /usage 보조 조회를 수집주기에도 실행합니다.",
    "advanced.indicator": "고급 조정",
    "action.refresh": "새로고침",
    "action.restore": "복원",
    "option.system": "시스템",
    "option.languageKo": "한국어",
    "option.languageEn": "English",
    "option.light": "라이트",
    "option.dark": "다크",
    "option.traffic": "신호등",
    "option.cvd": "색약",
    "option.cool": "쿨",
    "option.custom": "커스텀",
    "option.spacious": "넉넉",
    "option.compact": "컴팩트",
    "option.dual": "이중원",
    "option.quad": "링4",
    "option.primaryFirst": "5h 먼저",
    "option.secondaryFirst": "주간 먼저",
    "option.ring": "원",
    "option.bar": "바",
    "status.saving": "적용 중...",
    "status.saved": "자동 적용됨",
    "status.restored": "복원됨",
    "error.noTauri": "Tauri API를 사용할 수 없습니다",
  },
  en: {
    "aria.windowControls": "Window controls",
    "window.minimize": "Minimize",
    "window.maximize": "Maximize",
    "window.close": "Close",
    "scope.local": "Local",
    "limit.fiveHour": "5h",
    "limit.weekly": "Weekly",
    "reset.prefix": "Resets in",
    "reset.past": "Reset passed",
    "context.label": "Context",
    "state.stale": "stale",
    "meta.approx": "approximate",
    "empty.claude": "Run Juice, then use Claude once on this PC",
    "empty.codex": "Use Codex once on this PC to show data",
    "settings.title": "Settings",
    "section.appearance": "Appearance",
    "section.limits": "Remaining & collection",
    "section.taskbar": "Taskbar",
    "section.indicator": "Ring & bar details",
    "section.system": "System",
    "section.lab": "Lab",
    "field.theme": "Theme",
    "field.language": "Language",
    "field.font": "Font",
    "field.palette": "Palette",
    "field.safe": "Safe",
    "field.warning": "Warning",
    "field.danger": "Danger",
    "field.remainingWarning": "Remaining warning",
    "field.remainingDanger": "Remaining danger",
    "field.pollInterval": "Collection interval",
    "field.staleAfter": "Stale",
    "help.remainingThresholds": "The taskbar shows remaining usage, so warning and danger colors start at or below these remaining percentages.",
    "help.staleAfter": "Time after the last record before stale.",
    "unit.seconds": "s",
    "field.barMode": "Bar mode",
    "field.limitOrder": "Limit order",
    "field.indicatorStyle": "Indicator",
    "field.fullscreenHide": "Hide in fullscreen apps",
    "field.maximizedHide": "Hide under maximized windows",
    "field.showClaude": "Show Claude",
    "field.showCodex": "Show Codex",
    "field.ring": "Ring",
    "field.ringNumbers": "Ring numbers",
    "field.numberOutline": "Number outline",
    "field.numberOutlineWidth": "Number outline width",
    "field.ringSize": "Ring size",
    "field.ringThickness": "Ring thickness",
    "field.ringGap": "Ring gap",
    "field.centerGap": "Center gap",
    "field.ringNumberSize": "Ring number size",
    "field.ringNumberWeight": "Ring number weight",
    "field.infoTextSize": "Info text size",
    "field.infoTextWeight": "Info text weight",
    "field.autostart": "Autostart",
    "field.claudeUsageAutoRefresh": "Auto-refresh Claude /usage",
    "help.claudeUsageAutoRefresh": "Runs the supplemental Claude /usage lookup during the collection loop instead of manual refresh only.",
    "advanced.indicator": "Advanced tuning",
    "action.refresh": "Refresh",
    "action.restore": "Restore",
    "option.system": "System",
    "option.languageKo": "Korean",
    "option.languageEn": "English",
    "option.light": "Light",
    "option.dark": "Dark",
    "option.traffic": "Traffic",
    "option.cvd": "Color-blind safe",
    "option.cool": "Cool",
    "option.custom": "Custom",
    "option.spacious": "Spacious",
    "option.compact": "Compact",
    "option.dual": "Dual ring",
    "option.quad": "Ring 4",
    "option.primaryFirst": "5h first",
    "option.secondaryFirst": "Weekly first",
    "option.ring": "Ring",
    "option.bar": "Bar",
    "status.saving": "Applying...",
    "status.saved": "Applied automatically",
    "status.restored": "Restored",
    "error.noTauri": "Tauri API is unavailable",
  },
};

export function normalizeLanguage(value) {
  const language = String(value || "system").toLowerCase();
  return LANGUAGES.has(language) ? language : "system";
}

export function resolveLanguage(settingsOrLanguage = {}) {
  const selected =
    typeof settingsOrLanguage === "string"
      ? normalizeLanguage(settingsOrLanguage)
      : normalizeLanguage(settingsOrLanguage.language);
  if (selected !== "system") return selected;

  const systemLanguage = String(globalThis.navigator?.language || "").toLowerCase();
  return systemLanguage.startsWith("ko") ? "ko" : "en";
}

export function t(key, settingsOrLanguage = {}) {
  const language = resolveLanguage(settingsOrLanguage);
  return COPY[language]?.[key] ?? COPY.ko[key] ?? key;
}

export function formatDuration(minutes, settingsOrLanguage = {}) {
  const language = resolveLanguage(settingsOrLanguage);
  const safeMinutes = Math.max(0, Math.round(Number(minutes) || 0));
  const days = Math.floor(safeMinutes / 1440);
  const hours = Math.floor((safeMinutes % 1440) / 60);
  const mins = safeMinutes % 60;

  if (language === "en") {
    if (days > 0) return `${days}d ${hours}h`;
    return `${hours > 0 ? `${hours}h ` : ""}${mins}m`;
  }

  if (days > 0) return `${days}일 ${hours}시간`;
  return `${hours > 0 ? `${hours}시간 ` : ""}${mins}분`;
}

export function formatLocalDateTime(timestamp, settingsOrLanguage = {}) {
  const language = resolveLanguage(settingsOrLanguage);
  const locale = language === "ko" ? "ko-KR" : "en-US";
  return new Date(timestamp).toLocaleString(locale);
}

export function applyTranslations(
  settingsOrLanguage = {},
  root = globalThis.document?.documentElement,
) {
  if (!root) return resolveLanguage(settingsOrLanguage);

  const language = resolveLanguage(settingsOrLanguage);
  root.lang = language;
  if (typeof root.querySelectorAll !== "function") return language;

  for (const element of root.querySelectorAll("[data-i18n]")) {
    element.textContent = t(element.dataset.i18n, language);
  }
  for (const element of root.querySelectorAll("[data-i18n-title]")) {
    const value = t(element.dataset.i18nTitle, language);
    element.title = value;
    element.setAttribute("aria-label", value);
  }
  for (const element of root.querySelectorAll("[data-i18n-aria-label]")) {
    element.setAttribute("aria-label", t(element.dataset.i18nAriaLabel, language));
  }

  return language;
}
