import { type Message, rangeOf } from "@aqora/pisa-client";
import type { Diagnostic } from "@codemirror/lint";

const SEVERITY: Record<Message["severity"], Diagnostic["severity"]> = {
  trace: "hint",
  info: "info",
  warning: "warning",
  error: "error",
};

/** The messages of a check of `code` as editor diagnostics. They are only
 *  meaningful while the editor's text is `code`. */
export function toDiagnostics(code: string, messages: Message[]): Diagnostic[] {
  return messages.map((m) => {
    const { from, to } = rangeOf(code, m);
    return { from, to, severity: SEVERITY[m.severity], message: m.text };
  });
}
