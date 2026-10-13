export function createStatusRequests({
  invoke,
  getStatusGeneration,
  getHealthGeneration,
  acceptStatus,
  acceptHealth,
  statusTimeoutMs = 20_000,
  healthTimeoutMs = 2_000,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
}) {
  let sequence = 0;
  let disposed = false;
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

  return {
    async load() {
      if (disposed) return;
      const request = ++sequence;
      cancelWaits();
      const statusGeneration = getStatusGeneration();
      const healthGeneration = getHealthGeneration();
      const current = () => !disposed && request === sequence
        && statusGeneration === getStatusGeneration();
      try {
        const statuses = await waitFor(invoke("get_status"), statusTimeoutMs);
        if (!current()) return;
        try {
          const health = await waitFor(invoke("get_collection_health"), healthTimeoutMs);
          if (!current()) return;
          if (healthGeneration === getHealthGeneration()) acceptHealth(health);
        } catch {
          // Health is supplemental; a missing reply must not hide a valid quota.
        }
        if (current()) acceptStatus(statuses);
      } catch {
        if (current()) acceptStatus([]);
      }
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      sequence += 1;
      cancelWaits();
    },
  };
}
