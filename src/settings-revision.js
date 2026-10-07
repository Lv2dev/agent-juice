function snapshotRevision(snapshot) {
  const value = snapshot?.settings_revision;
  if (typeof value !== "string" || !/^(0|[1-9]\d{0,19})$/.test(value)) return null;
  const revision = BigInt(value);
  return revision <= 18446744073709551615n ? revision : null;
}

function validSnapshot(snapshot) {
  return snapshot && typeof snapshot === "object" && !Array.isArray(snapshot)
    && (!Object.prototype.hasOwnProperty.call(snapshot, "settings_revision")
      || snapshotRevision(snapshot) !== null);
}

export function createSettingsAuthority() {
  let current = null;
  let revision = null;
  const accept = (snapshot) => {
    if (!validSnapshot(snapshot)) return false;
    const next = snapshotRevision(snapshot);
    if (revision !== null && (next === null || next <= revision)) return false;
    current = snapshot;
    revision = next;
    return true;
  };
  return {
    accept,
    reply(snapshot, eventChanged = false) {
      if (!validSnapshot(snapshot)) return null;
      // Legacy replies cannot establish freshness after an intervening event.
      if (snapshotRevision(snapshot) !== null || !eventChanged) accept(snapshot);
      return current;
    },
    get current() { return current; },
  };
}
