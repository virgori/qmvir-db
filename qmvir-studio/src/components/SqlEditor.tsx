import { onMount, onCleanup, createEffect } from "solid-js";
import type { Component } from "solid-js";
import * as monaco from "monaco-editor";

// Configure Monaco environment for web workers
self.MonacoEnvironment = {
  getWorker: function () {
    return new Worker(
      new URL("monaco-editor/esm/vs/editor/editor.worker.js", import.meta.url),
      { type: "module" }
    );
  },
};

// SQL keywords for auto-complete
const SQL_KEYWORDS = [
  "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET",
  "DELETE", "CREATE", "TABLE", "DROP", "INDEX", "ALTER", "ADD", "COLUMN",
  "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON", "AND", "OR", "NOT",
  "IN", "BETWEEN", "LIKE", "IS", "NULL", "AS", "ORDER", "BY", "ASC", "DESC",
  "GROUP", "HAVING", "LIMIT", "OFFSET", "DISTINCT", "COUNT", "SUM", "AVG",
  "MIN", "MAX", "SHOW", "TABLES", "COLUMNS", "BEGIN", "COMMIT", "ROLLBACK",
  "EXPLAIN", "ANALYZE", "VACUUM", "COPY", "GRANT", "REVOKE", "INT", "BIGINT",
  "FLOAT", "DOUBLE", "TEXT", "VARCHAR", "BOOLEAN", "TIMESTAMP",
];

const SqlEditor: Component<{
  value: string;
  onChange: (val: string) => void;
  onExecute: () => void;
}> = (props) => {
  let containerRef: HTMLDivElement | undefined;
  let editor: monaco.editor.IStandaloneCodeEditor | undefined;

  onMount(() => {
    if (!containerRef) return;

    // Register SQL completion provider (once)
    monaco.languages.registerCompletionItemProvider("sql", {
      provideCompletionItems: (model, position) => {
        const word = model.getWordUntilPosition(position);
        const range = {
          startLineNumber: position.lineNumber,
          endLineNumber: position.lineNumber,
          startColumn: word.startColumn,
          endColumn: word.endColumn,
        };
        return {
          suggestions: SQL_KEYWORDS.map((kw) => ({
            label: kw,
            kind: monaco.languages.CompletionItemKind.Keyword,
            insertText: kw,
            range,
          })),
        };
      },
    });

    editor = monaco.editor.create(containerRef, {
      value: props.value,
      language: "sql",
      theme: "vs-dark",
      minimap: { enabled: false },
      fontSize: 13,
      fontFamily: "'SF Mono', 'Fira Code', 'Cascadia Code', monospace",
      lineNumbers: "on",
      renderLineHighlight: "line",
      scrollBeyondLastLine: false,
      wordWrap: "on",
      automaticLayout: true,
      padding: { top: 8, bottom: 8 },
      scrollbar: {
        verticalScrollbarSize: 8,
        horizontalScrollbarSize: 8,
      },
      overviewRulerLanes: 0,
      hideCursorInOverviewRuler: true,
      overviewRulerBorder: false,
      contextmenu: false,
      tabSize: 2,
      suggestOnTriggerCharacters: true,
    });

    // Sync changes
    editor.onDidChangeModelContent(() => {
      const val = editor!.getValue();
      props.onChange(val);
    });

    // Cmd+Enter to execute
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => {
      props.onExecute();
    });

    // Customize theme colors
    monaco.editor.defineTheme("qmvir-dark", {
      base: "vs-dark",
      inherit: true,
      rules: [],
      colors: {
        "editor.background": "#1a1d27",
        "editor.foreground": "#e2e8f0",
        "editorCursor.foreground": "#3b82f6",
        "editor.lineHighlightBackground": "#2a2d3a50",
        "editor.selectionBackground": "#3b82f640",
        "editorLineNumber.foreground": "#64748b",
        "editorLineNumber.activeForeground": "#e2e8f0",
      },
    });
    monaco.editor.setTheme("qmvir-dark");
  });

  // Update editor value when props.value changes externally
  createEffect(() => {
    const val = props.value;
    if (editor && editor.getValue() !== val) {
      editor.setValue(val);
    }
  });

  onCleanup(() => {
    editor?.dispose();
  });

  return (
    <div
      ref={containerRef}
      class="w-full h-full"
    />
  );
};

export default SqlEditor;
