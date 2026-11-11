import { type Diagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { useEffect, useRef } from "react";

import { lean } from "./lean-language.ts";

interface Props {
  value: string;
  onChange?: (value: string) => void;
  /** Replaces the editor's diagnostics when it changes. Between checks,
   *  CodeMirror moves the old ones along with edits. */
  diagnostics?: Diagnostic[] | undefined;
  /** The cursor's offset, as it moves. */
  onCursor?: (offset: number) => void;
  /** Ctrl/Cmd-Enter. */
  onRun?: () => void;
  readOnly?: boolean;
  className?: string;
  /** What the editor holds, for screen readers and tests. */
  label?: string;
}

/** A CodeMirror editor for Lean, controlled through `value`. */
export function LeanEditor({ value, onChange, diagnostics, onCursor, onRun, readOnly = false, className, label }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const callbacks = useRef({ onChange, onCursor, onRun });
  callbacks.current = { onChange, onCursor, onRun };

  useEffect(() => {
    if (host.current === null) return;
    const editor = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: value,
        extensions: [
          keymap.of([{ key: "Mod-Enter", run: () => (callbacks.current.onRun?.(), true) }]),
          basicSetup,
          lean,
          lintGutter(),
          EditorState.readOnly.of(readOnly),
          EditorView.contentAttributes.of(label !== undefined ? { "aria-label": label } : {}),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) callbacks.current.onChange?.(update.state.doc.toString());
            if (update.selectionSet || update.docChanged) {
              callbacks.current.onCursor?.(update.state.selection.main.head);
            }
          }),
        ],
      }),
    });
    view.current = editor;
    return () => {
      editor.destroy();
      view.current = null;
    };
    // The editor is made once; `value` and `diagnostics` reach it below.
  }, [readOnly, label]);

  useEffect(() => {
    const editor = view.current;
    if (editor === null || editor.state.doc.toString() === value) return;
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: value } });
  }, [value]);

  useEffect(() => {
    const editor = view.current;
    if (editor === null || diagnostics === undefined) return;
    editor.dispatch(setDiagnostics(editor.state, diagnostics));
  }, [diagnostics]);

  return <div ref={host} className={`overflow-hidden rounded-md border border-slate-300 bg-white ${className ?? ""}`} />;
}
