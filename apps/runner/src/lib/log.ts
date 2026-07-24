export interface LogLine {
  /** Seconds since the first timestamped line of the run. */
  offsetS: number;
  /** Wall-clock HH:MM:SS, for display. Empty for untimestamped lines. */
  time: string;
  level: "INFO" | "WARN";
  /** Last segment of the tracing target, e.g. "pipeline". */
  target: string;
  /** Everything after "target: " — message plus key=value fields, verbatim. */
  tail: string;
  message: string;
  fields: Record<string, string>;
}

const LINE_RE = /^(\S+Z)\s+(INFO|WARN)\s+(\S+):\s+(.*)$/;
const FIELD_RE = /([a-z0-9_]+)=(\S+)/g;

function parseTail(tail: string): { message: string; fields: Record<string, string> } {
  const fields: Record<string, string> = {};
  let messageEnd = tail.length;
  for (const match of tail.matchAll(FIELD_RE)) {
    if (match.index < messageEnd) messageEnd = match.index;
    fields[match[1]] = match[2];
  }
  return { message: tail.slice(0, messageEnd).trim(), fields };
}

/**
 * Parse a run's stderr into replayable lines. This is the same tracing
 * key=value grammar the live log drain will feed later; lines that don't
 * match (the final "Wrote …" summary has no timestamp) are kept verbatim
 * and inherit the previous line's offset.
 */
export function parseLog(text: string): LogLine[] {
  const lines: LogLine[] = [];
  let startMs: number | null = null;
  for (const raw of text.split("\n")) {
    if (raw.trim() === "") continue;
    const match = raw.match(LINE_RE);
    if (match) {
      const [, ts, level, target, tail] = match;
      const ms = Date.parse(ts);
      if (startMs === null) startMs = ms;
      const { message, fields } = parseTail(tail);
      lines.push({
        offsetS: (ms - startMs) / 1000,
        time: ts.slice(11, 19),
        level: level === "WARN" ? "WARN" : "INFO",
        target: target.split("::").pop() ?? target,
        tail,
        message,
        fields,
      });
    } else {
      lines.push({
        offsetS: lines.at(-1)?.offsetS ?? 0,
        time: "",
        level: "INFO",
        target: "",
        tail: raw,
        message: raw,
        fields: {},
      });
    }
  }
  return lines;
}
