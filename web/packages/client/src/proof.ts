import { posMapper } from "./positions.ts";
import type { Pos } from "./types.ts";

/** A `sorry` of a session's code, and the tactics that prove its goal. */
export interface SorryProof {
  /** Where the `sorry` is, as the session's states give it. */
  sorry: { pos?: Pos; end_pos?: Pos };
  tactics: string[];
}

/** `code` with each proved `sorry` replaced by its tactics, one per line and
 *  lined up with the first, for a session check. A `sorry` right after `:=`
 *  is taken for a term and gets `by` first; any other for a tactic. Sorries
 *  without tactics, or without a position, stay as they are. */
export function proofText(code: string, proofs: SorryProof[]): string {
  const map = posMapper(code);
  const spans = proofs
    .filter((p) => p.tactics.length > 0 && p.sorry.pos !== undefined)
    .map((p) => {
      const pos = p.sorry.pos as Pos;
      const from = map.toOffset(pos);
      const to = p.sorry.end_pos !== undefined ? map.toOffset(p.sorry.end_pos) : from + "sorry".length;
      return { from, to, column: pos.column, tactics: p.tactics };
    })
    .sort((a, b) => b.from - a.from);
  let out = code;
  for (const { from, to, column, tactics } of spans) {
    const term = /:=\s*$/.test(out.slice(0, from));
    const lead = term ? "by " : "";
    const indent = " ".repeat(column + lead.length);
    const lines = tactics.flatMap((t) => t.split("\n"));
    const text = lead + lines.map((line, i) => (i === 0 ? line : indent + line)).join("\n");
    out = out.slice(0, from) + text + out.slice(to);
  }
  return out;
}
