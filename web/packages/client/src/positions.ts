import type { Message, Pos } from "./types.ts";

/** Converts between Lean's positions in `code` (lines from 1, columns in
 *  code points) and offsets into the JavaScript string (UTF-16 units). */
export interface PosMapper {
  /** The offset of `pos`, clamped to the code: a line past the end is the
   *  end, a column past its line is the line's end. */
  toOffset(pos: Pos): number;
  toPos(offset: number): Pos;
}

export function posMapper(code: string): PosMapper {
  const starts = [0];
  for (let i = 0; i < code.length; i++) if (code[i] === "\n") starts.push(i + 1);
  const lineEnd = (index: number): number =>
    index + 1 < starts.length ? (starts[index + 1] as number) - 1 : code.length;
  return {
    toOffset({ line, column }) {
      if (line > starts.length) return code.length;
      const index = Math.max(0, line - 1);
      const end = lineEnd(index);
      let offset = starts[index] as number;
      for (let c = 0; c < column && offset < end; c++) offset += step(code, offset);
      return offset;
    },
    toPos(offset) {
      const at = Math.max(0, Math.min(offset, code.length));
      let lo = 0;
      let hi = starts.length - 1;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if ((starts[mid] as number) <= at) lo = mid;
        else hi = mid - 1;
      }
      let column = 0;
      for (let i = starts[lo] as number; i < at; i += step(code, i)) column++;
      return { line: lo + 1, column };
    },
  };
}

/** The UTF-16 units of the code point at `offset`. */
function step(code: string, offset: number): number {
  return (code.codePointAt(offset) ?? 0) > 0xffff ? 2 : 1;
}

const WORD = /[\p{L}\p{N}_.'!?₀-₉]/u;

/** The offsets a message covers in `code`. Without an end, it covers the
 *  word it starts at. */
export function rangeOf(code: string, message: Pick<Message, "pos" | "end_pos">): { from: number; to: number } {
  const map = posMapper(code);
  const from = map.toOffset(message.pos);
  if (message.end_pos !== undefined) return { from, to: Math.max(from, map.toOffset(message.end_pos)) };
  let to = from;
  while (to < code.length) {
    const c = String.fromCodePoint(code.codePointAt(to) ?? 0);
    if (!WORD.test(c)) break;
    to += c.length;
  }
  return { from, to };
}
