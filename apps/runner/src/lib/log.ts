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
 * A stateful line parser for the tracing key=value grammar: offsets are
 * measured from the first timestamped line it ever sees, so it can be fed
 * incrementally (the live CloudWatch tail) or in one shot (the fixture).
 * Lines that don't match (the final "Wrote …" summary has no timestamp) are
 * kept verbatim and inherit the previous line's offset. Blank lines yield null.
 */
export function createLogParser(): (raw: string) => LogLine | null {
  let startMs: number | null = null;
  let lastOffsetS = 0;
  return (raw: string) => {
    if (raw.trim() === "") return null;
    const match = raw.match(LINE_RE);
    if (!match) {
      return {
        offsetS: lastOffsetS,
        time: "",
        level: "INFO",
        target: "",
        tail: raw,
        message: raw,
        fields: {},
      };
    }
    const [, ts, level, target, tail] = match;
    const ms = Date.parse(ts);
    if (startMs === null) startMs = ms;
    lastOffsetS = (ms - startMs) / 1000;
    const { message, fields } = parseTail(tail);
    return {
      offsetS: lastOffsetS,
      time: ts.slice(11, 19),
      level: level === "WARN" ? "WARN" : "INFO",
      target: target.split("::").pop() ?? target,
      tail,
      message,
      fields,
    };
  };
}

/** Parse a whole run log at once (the checked-in demo fixture). */
export function parseLog(text: string): LogLine[] {
  const parse = createLogParser();
  const lines: LogLine[] = [];
  for (const raw of text.split("\n")) {
    const line = parse(raw);
    if (line) lines.push(line);
  }
  return lines;
}
