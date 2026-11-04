import { StreamLanguage, type StringStream } from "@codemirror/language";

const KEYWORDS = new Set(
  (
    "theorem lemma def abbrev example instance structure class inductive where deriving extends " +
    "namespace section end open import variable universe noncomputable private protected public " +
    "module meta by have show from fun let in match with if then else do return at calc " +
    "mutual axiom opaque set_option attribute local scoped macro syntax notation infix infixl infixr " +
    "prefix postfix termination_by decreasing_by"
  ).split(" "),
);

const TACTICS = new Set(
  (
    "intro intros exact apply refine rfl simp simp_all rw rewrite omega decide norm_num ring linarith " +
    "constructor cases rcases induction obtain use exists unfold trivial assumption contradiction " +
    "exfalso left right ext funext congr gcongr positivity aesop tauto skip first repeat all_goals " +
    "any_goals focus case next specialize generalize subst"
  ).split(" "),
);

interface State {
  /** Depth of `/- -/` comments, which nest. */
  comment: number;
}

function token(stream: StringStream, state: State): string | null {
  if (state.comment > 0) {
    while (!stream.eol()) {
      if (stream.match("/-")) state.comment++;
      else if (stream.match("-/")) {
        state.comment--;
        if (state.comment === 0) break;
      } else stream.next();
    }
    return "comment";
  }
  if (stream.eatSpace()) return null;
  if (stream.match("--")) {
    stream.skipToEnd();
    return "comment";
  }
  if (stream.match("/-")) {
    state.comment = 1;
    return token(stream, state) ?? "comment";
  }
  if (stream.match(/^"(?:[^"\\]|\\.)*"?/)) return "string";
  if (stream.match(/^\d+(?:\.\d+)?/)) return "number";
  if (stream.match(/^[\p{L}_][\p{L}\p{N}_.'!?₀-₉]*/u)) {
    const word = stream.current();
    if (word === "sorry" || word === "admit") return "invalid";
    if (KEYWORDS.has(word)) return "keyword";
    if (TACTICS.has(word)) return "builtin";
    if (/^[A-Z]/u.test(word)) return "typeName";
    return "variableName";
  }
  if (stream.match(/^(?::=|=>|->|←|→|↔|⟨|⟩|∀|∃|λ|·|<;>|[≤≥≠∧∨¬∘×])/u)) return "operator";
  stream.next();
  return null;
}

/** Lean 4, coarsely: comments, strings, numbers, keywords and tactics. */
export const lean = StreamLanguage.define<State>({
  name: "lean",
  startState: () => ({ comment: 0 }),
  copyState: (s) => ({ ...s }),
  token,
  languageData: { commentTokens: { line: "--", block: { open: "/-", close: "-/" } } },
});
