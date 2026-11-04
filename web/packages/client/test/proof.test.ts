import { describe, expect, it } from "vitest";

import { proofText } from "../src/index.ts";

const at = (line: number, column: number) => ({
  pos: { line, column },
  end_pos: { line, column: column + 5 },
});

describe("proofText", () => {
  it("lines tactics up with the first, in place of a tactic sorry", () => {
    const code = "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by\n  constructor\n  · sorry\n  · sorry\n";
    const text = proofText(code, [
      { sorry: at(3, 4), tactics: ["simp"] },
      { sorry: at(4, 4), tactics: ["induction n", "· rfl", "· simp"] },
    ]);
    expect(text).toBe(
      "theorem t (n : Nat) : n + 0 = n ∧ 0 + n = n := by\n" +
        "  constructor\n" +
        "  · simp\n" +
        "  · induction n\n" +
        "    · rfl\n" +
        "    · simp\n",
    );
  });

  it("adds `by` in place of a term sorry", () => {
    const code = "theorem t : 1 = 1 := sorry";
    expect(proofText(code, [{ sorry: at(1, 21), tactics: ["skip", "rfl"] }])).toBe(
      "theorem t : 1 = 1 := by skip\n                        rfl",
    );
  });

  it("leaves sorries without tactics or a position", () => {
    const code = "example : True := by sorry";
    expect(proofText(code, [{ sorry: at(1, 21), tactics: [] }, { sorry: {}, tactics: ["trivial"] }])).toBe(code);
  });
});
