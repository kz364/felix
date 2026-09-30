/* Bullet lists in a plain textarea: "- " or "* " at the start of a line
 * becomes "• ", Enter carries the list on, Enter on an empty item ends it,
 * and Tab / Shift+Tab indent. Pure functions over (text, cursor) so the
 * notes editor stays a textarea. */

export const BULLET = "• ";
const INDENT = "  ";

export type Edit = { text: string; cursor: number };

/** The line around `at`: where it starts and ends. */
const lineAt = (text: string, at: number) => {
  const start = text.lastIndexOf("\n", at - 1) + 1;
  const end = text.indexOf("\n", at);
  return { start, end: end === -1 ? text.length : end };
};

/** "  • ", "1. ", or null: the list marker a line starts with. */
const markerOf = (line: string) => {
  const m = /^(\s*)(• |(\d+)\. )/.exec(line);
  if (!m) return null;
  return { indent: m[1], marker: m[2], number: m[3] ? Number(m[3]) : null };
};

/** After typing: "- " or "* " at the start of a line becomes a bullet. */
export const afterInput = (text: string, cursor: number): Edit | null => {
  const { start } = lineAt(text, cursor);
  const before = text.slice(start, cursor);
  const m = /^(\s*)[-*] $/.exec(before);
  if (!m) return null;
  const replaced = m[1] + BULLET;
  return {
    text: text.slice(0, start) + replaced + text.slice(cursor),
    cursor: start + replaced.length,
  };
};

/** Enter inside a list item: a new item, or the end of the list if this
 * one is empty. Null leaves Enter alone. */
export const onEnter = (text: string, cursor: number): Edit | null => {
  const { start, end } = lineAt(text, cursor);
  const line = text.slice(start, end);
  const found = markerOf(line);
  if (!found) return null;
  const prefix = found.indent + found.marker;
  if (line.trim() === found.marker.trim()) {
    // An empty item: take the marker away and stop the list.
    return {
      text: text.slice(0, start) + text.slice(end),
      cursor: start,
    };
  }
  if (cursor < start + prefix.length) return null;
  const next =
    found.indent + (found.number === null ? BULLET : `${found.number + 1}. `);
  return {
    text: text.slice(0, cursor) + "\n" + next + text.slice(cursor),
    cursor: cursor + 1 + next.length,
  };
};

/** Tab / Shift+Tab on a list item: indent or outdent it. */
export const onTab = (
  text: string,
  cursor: number,
  outdent: boolean,
): Edit | null => {
  const { start, end } = lineAt(text, cursor);
  const line = text.slice(start, end);
  if (!markerOf(line)) return null;
  if (outdent) {
    if (!line.startsWith(INDENT)) return { text, cursor };
    return {
      text: text.slice(0, start) + line.slice(INDENT.length) + text.slice(end),
      cursor: Math.max(start, cursor - INDENT.length),
    };
  }
  return {
    text: text.slice(0, start) + INDENT + text.slice(start),
    cursor: cursor + INDENT.length,
  };
};

/** The bullet button: make the cursor's line an item, or plain again. */
export const toggleBullet = (text: string, cursor: number): Edit => {
  const { start, end } = lineAt(text, cursor);
  const line = text.slice(start, end);
  const found = markerOf(line);
  if (found) {
    const drop = found.indent.length + found.marker.length;
    return {
      text: text.slice(0, start) + line.slice(drop) + text.slice(end),
      cursor: Math.max(start, cursor - drop),
    };
  }
  return {
    text: text.slice(0, start) + BULLET + text.slice(start),
    cursor: cursor + BULLET.length,
  };
};
