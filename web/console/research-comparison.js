/* Exact timestamp and integer-equity alignment for cross-run research comparisons. */
(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.QxResearchComparison = api;
})(typeof globalThis === "object" ? globalThis : this, function () {
  const MIN_EPOCH_MS = 100_000_000_000n;
  const MAX_DATE_MS = 8_640_000_000_000_000n;

  function parseTimestamp(value) {
    if (typeof value !== "string" && typeof value !== "number") return null;
    const text = String(value);
    if (!/^\d+$/.test(text)) return null;
    try {
      const timestamp = BigInt(text);
      return timestamp >= MIN_EPOCH_MS && timestamp <= MAX_DATE_MS ? timestamp : null;
    } catch {
      return null;
    }
  }

  function parseEquityRows(rows) {
    if (!Array.isArray(rows) || rows.length < 2) return null;
    const points = [];
    let previousTimestamp = null;
    for (const row of rows) {
      if (!row || typeof row !== "object") return null;
      const timestamp = parseTimestamp(row.ts);
      if (timestamp === null || (previousTimestamp !== null && timestamp <= previousTimestamp)) return null;
      let equity;
      try {
        if (typeof row.equity_raw !== "string" && typeof row.equity_raw !== "number") return null;
        equity = BigInt(String(row.equity_raw));
      } catch {
        return null;
      }
      points.push({ timestamp, equity });
      previousTimestamp = timestamp;
    }
    return points;
  }

  function validateUtcEquityRows(rows) {
    return parseEquityRows(rows) ? { valid: true } : { valid: false };
  }

  function equityAtOrBefore(points, timestamp) {
    let low = 0;
    let high = points.length;
    while (low < high) {
      const middle = low + Math.floor((high - low) / 2);
      if (points[middle].timestamp <= timestamp) low = middle + 1;
      else high = middle;
    }
    return low === 0 ? null : points[low - 1].equity;
  }

  function alignUtcEquityCurves(curves) {
    if (!Array.isArray(curves) || curves.length < 2) return { ok: false, reason: "need-more-curves" };
    const parsed = [];
    for (const curve of curves) {
      const points = parseEquityRows(curve.rows);
      if (!points) return { ok: false, reason: "invalid-timestamp-or-equity" };
      parsed.push({ curve, points });
    }

    const start = parsed.reduce((maximum, item) =>
      item.points[0].timestamp > maximum ? item.points[0].timestamp : maximum,
    parsed[0].points[0].timestamp);
    const end = parsed.reduce((minimum, item) => {
      const last = item.points[item.points.length - 1].timestamp;
      return last < minimum ? last : minimum;
    }, parsed[0].points[parsed[0].points.length - 1].timestamp);
    if (start >= end) return { ok: false, reason: "no-common-time-range" };

    const axis = new Set([start.toString(), end.toString()]);
    for (const item of parsed) {
      for (const point of item.points) {
        if (point.timestamp >= start && point.timestamp <= end) axis.add(point.timestamp.toString());
      }
    }
    const timestamps = [...axis].map((value) => BigInt(value)).sort((left, right) =>
      left < right ? -1 : left > right ? 1 : 0);

    const aligned = [];
    for (const item of parsed) {
      const baseline = equityAtOrBefore(item.points, start);
      if (baseline === null || baseline === 0n) return { ok: false, reason: "zero-or-missing-overlap-baseline" };
      const denominator = baseline < 0n ? -baseline : baseline;
      const values = timestamps.map((timestamp) => {
        const current = equityAtOrBefore(item.points, timestamp);
        return Number(((current - baseline) * 10_000n) / denominator);
      });
      aligned.push({ runId: item.curve.runId, values });
    }
    return {
      ok: true,
      startTimestamp: start.toString(),
      endTimestamp: end.toString(),
      timestamps: timestamps.map((timestamp) => timestamp.toString()),
      curves: aligned,
    };
  }

  function parseSafeInteger(value) {
    if (typeof value !== "string" && typeof value !== "number") return null;
    if (typeof value === "number" && !Number.isSafeInteger(value)) return null;
    if (!/^-?\d+$/.test(String(value))) return null;
    try {
      return BigInt(String(value));
    } catch {
      return null;
    }
  }

  function unifiedEventKey(row, reportId) {
    if (!row || typeof row !== "object" || typeof reportId !== "string" || !reportId) return null;
    const timestamp = parseSafeInteger(row.ts);
    const priority = parseSafeInteger(row.prio ?? row.priority);
    const sequence = parseSafeInteger(row.seq);
    if (timestamp === null || priority === null || sequence === null) return null;
    return {
      timestamp,
      priority,
      reportId,
      accountId: typeof row.account_id === "string" ? row.account_id : "",
      sequence,
    };
  }

  function compareUnicodeStrings(left, right) {
    const a = left[Symbol.iterator]();
    const b = right[Symbol.iterator]();
    while (true) {
      const nextA = a.next();
      const nextB = b.next();
      if (nextA.done || nextB.done) {
        return nextA.done === nextB.done ? 0 : nextA.done ? -1 : 1;
      }
      const pointA = nextA.value.codePointAt(0);
      const pointB = nextB.value.codePointAt(0);
      if (pointA !== pointB) return pointA < pointB ? -1 : 1;
    }
  }

  function compareUnifiedEventKeys(left, right) {
    if (left.timestamp !== right.timestamp) return left.timestamp < right.timestamp ? -1 : 1;
    if (left.priority !== right.priority) return left.priority < right.priority ? -1 : 1;
    const report = compareUnicodeStrings(left.reportId, right.reportId);
    if (report) return report;
    const account = compareUnicodeStrings(left.accountId, right.accountId);
    if (account) return account;
    if (left.sequence !== right.sequence) return left.sequence < right.sequence ? -1 : 1;
    return 0;
  }

  /*
   * Return only the prefix whose global order is proven by every source cursor. A fetched tail
   * later than the requested boundary is a watermark: all unseen events in that source follow it.
   * Rows from a failed or incomplete source are never silently treated as the end of that source.
   */
  function verifiedUnifiedEventPrefix(streams, requestedCount) {
    if (!Array.isArray(streams) || !Number.isSafeInteger(requestedCount) || requestedCount < 0) {
      return { valid: false, safe: false, safeCount: 0, rows: [], needsMore: [] };
    }
    const normalized = [];
    const entries = [];
    const seenKeys = new Set();
    const seenRunIds = new Set();
    for (const stream of streams) {
      if (!stream || typeof stream.key !== "string" || typeof stream.runId !== "string"
        || !stream.key || !stream.runId || seenKeys.has(stream.key) || seenRunIds.has(stream.runId)
        || !Array.isArray(stream.rows)) {
        return { valid: false, safe: false, safeCount: 0, rows: [], needsMore: [] };
      }
      seenKeys.add(stream.key);
      seenRunIds.add(stream.runId);
      const keyed = [];
      for (const row of stream.rows) {
        const key = unifiedEventKey(row, stream.runId);
        if (!key || (keyed.length && compareUnifiedEventKeys(keyed[keyed.length - 1].key, key) >= 0)) {
          return { valid: false, safe: false, safeCount: 0, rows: [], needsMore: [] };
        }
        const entry = { row, key, runId: stream.runId };
        keyed.push(entry);
        entries.push(entry);
      }
      normalized.push({ ...stream, keyed, tail: keyed.at(-1)?.key ?? null });
    }
    entries.sort((left, right) => compareUnifiedEventKeys(left.key, right.key));
    for (let index = 1; index < entries.length; index += 1) {
      if (compareUnifiedEventKeys(entries[index - 1].key, entries[index].key) >= 0) {
        return { valid: false, safe: false, safeCount: 0, rows: [], needsMore: [] };
      }
    }
    const isComplete = (stream) => stream.complete === true;
    let safeCount = 0;
    const maxCheck = Math.min(requestedCount, entries.length);
    for (let index = 0; index < maxCheck; index += 1) {
      const boundary = entries[index].key;
      const unseenCouldPrecede = normalized.some((stream) =>
        !isComplete(stream) && (!stream.tail || compareUnifiedEventKeys(stream.tail, boundary) < 0));
      if (unseenCouldPrecede) break;
      safeCount += 1;
    }
    const allComplete = normalized.every(isComplete);
    if (allComplete) safeCount = entries.length;
    let needsMore = [];
    if (!allComplete && safeCount < requestedCount) {
      const boundary = entries[Math.min(requestedCount, entries.length) - 1]?.key ?? null;
      needsMore = normalized
        .filter((stream) => !isComplete(stream)
          && (!boundary || !stream.tail || compareUnifiedEventKeys(stream.tail, boundary) < 0))
        .map((stream) => stream.key);
      if (entries.length < requestedCount) {
        needsMore = normalized.filter((stream) => !isComplete(stream)).map((stream) => stream.key);
      }
    }
    const safe = allComplete || safeCount >= requestedCount;
    return {
      valid: true,
      safe,
      safeCount,
      rows: entries.slice(0, safeCount).map((entry) => ({ ...entry.row, report_id: entry.runId })),
      needsMore,
    };
  }

  function filterUnifiedEventRows(rows, filters = {}, query = "") {
    if (!Array.isArray(rows)) return [];
    const normalizedQuery = String(query).trim().toLocaleLowerCase();
    return rows.filter((row) => Object.entries(filters).every(([field, selected]) =>
      selected == null || selected === "" || String(row?.[field] ?? "") === String(selected)
    )).filter((row) => !normalizedQuery || Object.values(row ?? {})
      .some((value) => String(value ?? "").toLocaleLowerCase().includes(normalizedQuery)));
  }

  function unifiedEventCsv(rows, columns) {
    if (!Array.isArray(rows) || !Array.isArray(columns)) return "";
    const cell = (value) => {
      const raw = value && typeof value === "object" ? JSON.stringify(value) : String(value ?? "");
      const negativeNumeric = /^-\d+(?:\.\d+)?$/.test(raw);
      const safe = /^[=+@\t\r]/.test(raw) || (raw.startsWith("-") && !negativeNumeric)
        ? `'${raw}`
        : raw;
      return `"${safe.replaceAll('"', '""')}"`;
    };
    return [columns, ...rows.map((row) => columns.map((column) => row?.[column]))]
      .map((row) => row.map(cell).join(","))
      .join("\r\n");
  }

  return {
    validateUtcEquityRows,
    alignUtcEquityCurves,
    verifiedUnifiedEventPrefix,
    filterUnifiedEventRows,
    unifiedEventCsv,
  };
});
