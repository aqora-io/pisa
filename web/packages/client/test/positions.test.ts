import { describe, expect, it } from "vitest";

import { posMapper, rangeOf } from "../src/index.ts";

describe("posMapper", () => {
  // `𝕜` is outside the BMP: one code point, two UTF-16 units.
  const code = "variable (𝕜 : Type)\n  example : 𝕜 = 𝕜 := rfl\n";
  const map = posMapper(code);

  it("counts columns in code points", () => {
    const at = map.toOffset({ line: 2, column: 16 });
    expect(code.slice(at, at + 2)).toBe("𝕜");
    expect(map.toPos(at)).toEqual({ line: 2, column: 16 });
    expect(code.slice(map.toOffset({ line: 1, column: 12 }))).toMatch(/^: Type/);
  });

  it("clamps what is past a line or the code", () => {
    expect(map.toOffset({ line: 1, column: 99 })).toBe(code.indexOf("\n"));
    expect(map.toOffset({ line: 9, column: 0 })).toBe(code.length);
    expect(map.toPos(-1)).toEqual({ line: 1, column: 0 });
    expect(map.toPos(code.length + 5)).toEqual({ line: 3, column: 0 });
  });

  it("covers the word a message starts at when it has no end", () => {
    const { from, to } = rangeOf(code, { pos: { line: 2, column: 2 } });
    expect(code.slice(from, to)).toBe("example");
    const ranged = rangeOf(code, { pos: { line: 2, column: 12 }, end_pos: { line: 2, column: 13 } });
    expect(code.slice(ranged.from, ranged.to)).toBe("𝕜");
  });
});
