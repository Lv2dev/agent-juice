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
    "reset.past": "초기화 시간 경과",
    "context.label": "컨텍스트",
    "state.stale": "오래됨",
    "meta.approx": "근사치",
    "empty.claude": "Juice 실행 후 Claude를 한 번 사용하면 표시됩니다",
    "empty.claudeCollect": "Claude 계정 사용량을 수집 중입니다. 채팅을 보낼 필요가 없습니다.",
    "empty.codex": "Codex를 이 PC에서 한 번 사용하면 표시됩니다",
    "settings.title": "설정",
    "section.appearance": "외형",
    "section.limits": "표시·수집",
    "section.taskbar": "작업표시줄",
    "section.indicator": "원·바 세부",
    "section.system": "시스템",
    "section.update": "업데이트",
    "section.about": "정보",
    "field.theme": "테마",
    "field.language": "언어",
    "field.font": "폰트",
    "field.palette": "팔레트",
    "field.monoColor": "통일 색상",
    "field.toolColors": "도구별 색상",
    "field.claudePrimaryColor": "Claude 5h",
    "field.claudeSecondaryColor": "Claude 주간",
    "field.codexPrimaryColor": "Codex 5h",
    "field.codexSecondaryColor": "Codex 주간",
    "field.safe": "안전",
    "field.warning": "경고",
    "field.danger": "위험",
    "field.displayBasis": "표시 기준",
    "field.remainingWarning": "잔여량 경고",
    "field.remainingDanger": "잔여량 위험",
    "field.usedWarning": "사용량 경고",
    "field.usedDanger": "사용량 위험",
    "field.pollInterval": "수집주기",
    "field.staleAfter": "오래됨",
    "help.remainingThresholds": "작업표시줄은 남은 사용량을 표시하므로, 이 값 이하가 되면 경고/위험 색을 씁니다.",
    "help.usedThresholds": "표시 사용량이 이 값 이상이 되면 경고/위험 색을 씁니다.",
    "help.pollInterval": "로컬 상태를 다시 읽는 간격.",
    "help.staleAfter": "마지막 기록 후 오래됨 표시까지.",
    "unit.seconds": "초",
    "field.barMode": "바 모드",
    "field.fullResetTime": "Full 리셋 시간",
    "field.limitOrder": "한도 순서",
    "field.indicatorStyle": "표시",
    "field.effectStyle": "표현 스타일",
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
    "field.centerSize": "중앙 공간 지름",
    "field.ringNumberSize": "원 안 숫자 크기",
    "field.ringNumberWeight": "원 안 숫자 굵기",
    "field.infoTextSize": "바깥 정보 글자 크기",
    "field.infoTextWeight": "바깥 정보 글자 굵기",
    "field.barContentGap": "표시기-정보 간격",
    "field.autostart": "자동시작",
    "field.updateCheck": "업데이트 자동 확인",
    "field.claudeUsageAutoRefresh": "Claude 계정 사용량 자동 수집",
    "help.claudeUsageAutoRefresh": "Claude Code 로컬 로그인으로 계정 사용량을 조회합니다. 채팅을 보내지 않으며 실패 시 기존 statusline 값을 유지합니다.",
    "help.updateCheck": "시작 후 하루에 한 번 최신 정식 릴리즈를 확인합니다.",
    "advanced.typography": "글자 조정",
    "advanced.indicator": "고급 조정",
    "action.refresh": "새로고침",
    "action.restore": "복원",
    "action.checkUpdate": "업데이트 확인",
    "action.releasePage": "릴리즈 페이지",
    "action.viewRelease": "릴리즈 보기",
    "effect.flat": "플랫",
    "effect.soft": "소프트 그림자",
    "effect.depth": "입체",
    "effect.glow": "글로우",
    "effect.breathe": "숨쉬기",
    "about.version": "버전",
    "about.description": "Claude Code와 Codex의 5시간·주간 사용량을 Windows 작업표시줄에서 확인하는 로컬 모니터입니다.",
    "about.privacy": "Juice는 사용량이나 로그인 정보를 별도 Juice 서버에 저장하지 않습니다.",
    "update.available": "새 버전을 사용할 수 있습니다",
    "status.updateChecking": "업데이트를 확인하는 중...",
    "status.updateCurrent": "최신 버전을 사용 중입니다.",
    "status.updateAvailable": "새 버전을 사용할 수 있습니다.",
    "status.updateFailed": "업데이트를 확인하지 못했습니다. 잠시 후 다시 시도하세요.",
    "status.updateUnknown": "아직 업데이트를 확인하지 않았습니다.",
    "option.system": "시스템",
    "option.languageKo": "한국어",
    "option.languageEn": "English",
    "option.light": "라이트",
    "option.dark": "다크",
    "option.tool": "도구별",
    "option.signal": "신호등",
    "option.ocean": "바다",
    "option.forest": "숲",
    "option.sunset": "노을",
    "option.cvd": "색각 보정",
    "option.cool": "오로라",
    "option.mono": "단색",
    "option.custom": "사용자 지정",
    "option.spacious": "넉넉",
    "option.compact": "컴팩트",
    "option.dual": "이중원",
    "option.quad": "링4",
    "option.primaryFirst": "5h 먼저",
    "option.secondaryFirst": "주간 먼저",
    "option.ring": "원",
    "option.bar": "바",
    "option.remaining": "잔여량",
    "option.used": "사용량",
    "status.saving": "적용 중...",
    "status.saved": "적용 완료",
    "status.savedRetrying": "저장 완료 · 시스템 적용 재시도 중",
    "status.restored": "복원됨",
    "status.settingsLoadFailed": "설정을 불러오지 못했습니다. 잠시 후 창을 다시 여세요.",
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
    "empty.claudeCollect": "Collecting Claude account usage. No chat message is required.",
    "empty.codex": "Use Codex once on this PC to show data",
    "settings.title": "Settings",
    "section.appearance": "Appearance",
    "section.limits": "Display & collection",
    "section.taskbar": "Taskbar",
    "section.indicator": "Ring & bar details",
    "section.system": "System",
    "section.update": "Updates",
    "section.about": "About",
    "field.theme": "Theme",
    "field.language": "Language",
    "field.font": "Font",
    "field.palette": "Palette",
    "field.monoColor": "Unified color",
    "field.toolColors": "Tool colors",
    "field.claudePrimaryColor": "Claude 5h",
    "field.claudeSecondaryColor": "Claude weekly",
    "field.codexPrimaryColor": "Codex 5h",
    "field.codexSecondaryColor": "Codex weekly",
    "field.safe": "Safe",
    "field.warning": "Warning",
    "field.danger": "Danger",
    "field.displayBasis": "Display basis",
    "field.remainingWarning": "Remaining warning",
    "field.remainingDanger": "Remaining danger",
    "field.usedWarning": "Usage warning",
    "field.usedDanger": "Usage danger",
    "field.pollInterval": "Collection interval",
    "field.staleAfter": "Stale",
    "help.remainingThresholds": "The taskbar shows remaining usage, so warning and danger colors start at or below these remaining percentages.",
    "help.usedThresholds": "Warning and danger colors start at or above these displayed usage percentages.",
    "help.pollInterval": "How often Juice rereads local status.",
    "help.staleAfter": "Time after the last record before stale.",
    "unit.seconds": "s",
    "field.barMode": "Bar mode",
    "field.fullResetTime": "Reset time in Full",
    "field.limitOrder": "Limit order",
    "field.indicatorStyle": "Indicator",
    "field.effectStyle": "Visual style",
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
    "field.centerSize": "Center opening",
    "field.ringNumberSize": "Inside number size",
    "field.ringNumberWeight": "Inside number weight",
    "field.infoTextSize": "Outer info text size",
    "field.infoTextWeight": "Outer info text weight",
    "field.barContentGap": "Indicator-to-info gap",
    "field.autostart": "Autostart",
    "field.updateCheck": "Automatically check for updates",
    "field.claudeUsageAutoRefresh": "Auto-collect Claude account usage",
    "help.claudeUsageAutoRefresh": "Reads account usage through the local Claude Code login without sending a chat and keeps existing statusline values if it fails.",
    "help.updateCheck": "Checks the latest stable release once a day after startup.",
    "advanced.typography": "Typography",
    "advanced.indicator": "Advanced tuning",
    "action.refresh": "Refresh",
    "action.restore": "Restore",
    "action.checkUpdate": "Check for updates",
    "action.releasePage": "Releases",
    "action.viewRelease": "View release",
    "effect.flat": "Flat",
    "effect.soft": "Soft shadow",
    "effect.depth": "Depth",
    "effect.glow": "Glow",
    "effect.breathe": "Breathe",
    "about.version": "Version",
    "about.description": "A local Windows taskbar monitor for Claude Code and Codex five-hour and weekly usage.",
    "about.privacy": "Juice does not store usage or login data on a separate Juice server.",
    "update.available": "A new version is available",
    "status.updateChecking": "Checking for updates...",
    "status.updateCurrent": "You are using the latest version.",
    "status.updateAvailable": "A new version is available.",
    "status.updateFailed": "Could not check for updates. Try again shortly.",
    "status.updateUnknown": "Updates have not been checked yet.",
    "option.system": "System",
    "option.languageKo": "Korean",
    "option.languageEn": "English",
    "option.light": "Light",
    "option.dark": "Dark",
    "option.tool": "Per tool",
    "option.signal": "Traffic",
    "option.ocean": "Ocean",
    "option.forest": "Forest",
    "option.sunset": "Sunset",
    "option.cvd": "Color-blind safe",
    "option.cool": "Aurora",
    "option.mono": "Monochrome",
    "option.custom": "Custom",
    "option.spacious": "Spacious",
    "option.compact": "Compact",
    "option.dual": "Dual ring",
    "option.quad": "Ring 4",
    "option.primaryFirst": "5h first",
    "option.secondaryFirst": "Weekly first",
    "option.ring": "Ring",
    "option.bar": "Bar",
    "option.remaining": "Remaining",
    "option.used": "Usage",
    "status.saving": "Applying...",
    "status.saved": "Applied",
    "status.savedRetrying": "Saved · retrying system apply",
    "status.restored": "Restored",
    "status.settingsLoadFailed": "Could not load settings. Reopen the window shortly.",
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
