export function createStatusRequests({
  invoke,
  getStatusGeneration,
  getHealthGeneration,
  acceptStatus,
  acceptHealth,
  statusTimeoutMs = 20_000,
  refreshTimeoutMs = 46_000,
  healthTimeoutMs = 2_000,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
}) {
  let sequence = 0;
  let disposed = false;
  let refreshInFlight = null;
  const pendingWaits = new Set();

  function cancelWaits() {
    for (const cancel of [...pendingWaits]) cancel();
  }

  function waitFor(promise, timeoutMs) {
    return new Promise((resolve, reject) => {
      let finished = false;
      let timer;
      const settle = (complete, value) => {
        if (finished) return;
        finished = true;
        clearTimer(timer);
        pendingWaits.delete(cancel);
        complete(value);
      };
      const cancel = () => settle(reject, new Error("status request retired"));
      pendingWaits.add(cancel);
      timer = setTimer(() => settle(reject, new Error("status request timed out")), timeoutMs);
      timer?.unref?.();
      Promise.resolve(promise).then(
        value => settle(resolve, value),
        error => settle(reject, error),
      );
    });
  }

  async function requestStatus(force) {
    const request = ++sequence;
    cancelWaits();
    const statusGeneration = getStatusGeneration();
    const healthGeneration = getHealthGeneration();
    const requestCurrent = () => !disposed && request === sequence;
    const statusCurrent = () => requestCurrent() && statusGeneration === getStatusGeneration();
    const healthCurrent = () => requestCurrent() && healthGeneration === getHealthGeneration();
    const syncHealth = async (renderCurrent = false) => {
      if (!healthCurrent()) return;
      try {
        const health = await waitFor(invoke("get_collection_health"), healthTimeoutMs);
        if (healthCurrent()) acceptHealth(health, renderCurrent || !statusCurrent());
      } catch {
        // Health is supplemental; a missing reply must not hide a valid quota.
      }
    };
    try {
      const statuses = await waitFor(
        invoke(force ? "refresh_status" : "get_status"),
        force ? refreshTimeoutMs : statusTimeoutMs,
      );
      if (!requestCurrent()) return { ok: false, retired: true };
      if (force && !Array.isArray(statuses)) throw new Error("invalid refresh response");
      await syncHealth();
      if (!requestCurrent()) return { ok: false, retired: true };
      if (statusCurrent()) acceptStatus(statuses);
      return { ok: true };
    } catch (error) {
      if (statusCurrent() && !force) acceptStatus([]);
      // A status event can contain only partial results from this same manual request.
      if (force) await syncHealth(true);
      if (force && requestCurrent() && error === "usage_refresh_throttled") {
        return { ok: false, throttled: true };
      }
      return { ok: false, retired: force ? !requestCurrent() : !statusCurrent() };
    }
  }

  function load(force = false) {
    if (disposed) return Promise.resolve({ ok: false, retired: true });
    if (refreshInFlight) return refreshInFlight;
    const pending = requestStatus(force);
    if (!force) return pending;
    const tracked = pending.finally(() => {
      if (refreshInFlight === tracked) refreshInFlight = null;
    });
    refreshInFlight = tracked;
    return tracked;
  }

  return {
    load: () => load(),
    refresh: () => load(true),
    dispose() {
      if (disposed) return;
      disposed = true;
      sequence += 1;
      cancelWaits();
    },
  };
}
