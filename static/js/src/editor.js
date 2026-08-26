/*
 * The Recipe editor.
 *
 * Adapted from CookCLI (static/js/src/editor.js): the extension list, the
 * highlight style, and the base theme are theirs.
 *
 * Copyright (c) 2021-2023 Alexey Dubovskoy
 * Licensed under the MIT License. See LICENSE-MIT-cookcli.
 *
 * Used here so that CookLangHub and CookCLI read as one family. See NOTICE.
 *
 * Four things differ from CookCLI.
 *
 * 1. There is no language server, so the completion source and the linter
 *    of CookCLI are absent. The Cooklang messages come from the Rust
 *    parser through the preview instead.
 * 2. The preview is server-rendered. This project has exactly one Cooklang
 *    parser, the Rust one, and a second parser in this file would drift
 *    from it. The editor posts the source to the application and puts the
 *    answer on the page.
 * 3. The palette comes from the class that the server wrote on <html>, or
 *    from the operating system when the person made no choice. CodeMirror
 *    takes its theme in JavaScript, so the page cannot do this in CSS
 *    alone.
 * 4. The editor lives in a shadow root. CodeMirror puts its own CSS on the
 *    page as it starts, and the Content Security Policy of this
 *    application stops a style that arrives that way. A shadow root takes
 *    the same CSS as an adopted stylesheet, which the policy allows.
 *
 * The page works without this file. The plain text area carries the source,
 * and the preview arrives with the page.
 */

import { EditorState } from "@codemirror/state";
import {
  EditorView,
  keymap,
  lineNumbers,
  highlightActiveLine,
  highlightActiveLineGutter,
} from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import {
  syntaxHighlighting,
  HighlightStyle,
  bracketMatching,
} from "@codemirror/language";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import { tags as t } from "@lezer/highlight";
import { cooklang } from "./cooklang-mode.js";

/* How long to wait after the last keystroke before the preview is asked for. */
const PREVIEW_DELAY_MS = 350;

/* One colour per Cooklang entity, the CookCLI mapping. */
const LIGHT_HIGHLIGHT = HighlightStyle.define([
  { tag: t.variableName, color: "#ea580c", fontWeight: "600" }, // Ingredients
  { tag: t.keyword, color: "#16a34a", fontWeight: "600" }, // Cookware
  { tag: t.number, color: "#dc2626", fontWeight: "600" }, // Timers
  { tag: t.comment, color: "#9ca3af", fontStyle: "italic" }, // Comments
  { tag: t.meta, color: "#8b5cf6" }, // Metadata
  { tag: t.unit, color: "#6366f1" }, // Units
  { tag: t.heading, color: "#0891b2", fontWeight: "700", fontSize: "1.1em" },
  { tag: t.string, color: "#d97706", fontStyle: "italic" }, // Preparations
]);

/*
 * The same hues, one step lighter, because a dark background needs it.
 * Every value is the dark step of the CookCLI colour ramp.
 */
const DARK_HIGHLIGHT = HighlightStyle.define([
  { tag: t.variableName, color: "#fb923c", fontWeight: "600" },
  { tag: t.keyword, color: "#4ade80", fontWeight: "600" },
  { tag: t.number, color: "#f87171", fontWeight: "600" },
  { tag: t.comment, color: "#6b7280", fontStyle: "italic" },
  { tag: t.meta, color: "#a78bfa" },
  { tag: t.unit, color: "#818cf8" },
  { tag: t.heading, color: "#22d3ee", fontWeight: "700", fontSize: "1.1em" },
  { tag: t.string, color: "#fbbf24", fontStyle: "italic" },
]);

/* Layout only. CookCLI has this theme, with the height made flexible. */
function baseTheme(dark) {
  return EditorView.theme(
    {
      "&": {
        /* CookCLI fixes the height. Here the editor grows with the Recipe. */
        height: "auto",
        minHeight: "24rem",
        fontSize: "14px",
        backgroundColor: dark ? "#111827" : "#ffffff",
        color: dark ? "#f3f4f6" : "#111827",
        borderRadius: "0.75rem",
      },
      "&.cm-focused": { outline: "2px solid #ff6b35" },
      ".cm-scroller": {
        fontFamily:
          "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
        overflow: "auto",
        borderRadius: "0.75rem",
      },
      ".cm-content": { padding: "1rem", caretColor: dark ? "#ffffff" : "#111827" },
      ".cm-line": { padding: "0 0.5rem" },
      ".cm-gutters": {
        backgroundColor: dark ? "#1f2937" : "#f9fafb",
        color: dark ? "#9ca3af" : "#6b7280",
        border: "none",
        borderTopLeftRadius: "0.75rem",
        borderBottomLeftRadius: "0.75rem",
      },
      ".cm-activeLine": { backgroundColor: dark ? "#1f2937" : "#fff7ed" },
      ".cm-activeLineGutter": { backgroundColor: dark ? "#374151" : "#ffedd5" },
    },
    { dark },
  );
}

/*
 * Which palette to draw in.
 *
 * The server writes `dark` or `light` on <html> when the person chose one.
 * It writes neither when they follow the operating system, and then the
 * browser answers.
 */
function isDark() {
  const root = document.documentElement;
  if (root.classList.contains("dark")) return true;
  if (root.classList.contains("light")) return false;
  return (
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
  );
}

/*
 * Ask the application to render the source and put the answer on the page.
 *
 * Only the newest answer is used. A slow answer to an older source must not
 * replace a newer preview.
 */
function makePreview(target, url) {
  let sequence = 0;
  let timer = null;

  async function send(source) {
    const mine = ++sequence;
    try {
      const response = await fetch(url, {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ source }).toString(),
      });
      if (!response.ok) return;
      const html = await response.text();
      if (mine !== sequence) return;
      target.innerHTML = html;
    } catch (error) {
      /* The preview is help, not the Recipe. A failure changes nothing. */
    }
  }

  return function schedule(source) {
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => send(source), PREVIEW_DELAY_MS);
  };
}

function start() {
  const host = document.getElementById("recipe-editor");
  const source = document.getElementById("recipe-source");
  if (!host || !source) return;

  /*
   * CodeMirror carries its own CSS and puts it on the page as it starts.
   * The Content Security Policy of this application is `default-src 'self'`,
   * which stops a style that arrives inside the page. A shadow root gets
   * the same CSS as an adopted stylesheet instead, and the policy allows
   * that, so the editor is styled and the policy stays as it is.
   *
   * A browser that cannot make a shadow root keeps the plain text area,
   * which works on its own.
   */
  if (typeof host.attachShadow !== "function") return;
  const room = host.shadowRoot || host.attachShadow({ mode: "open" });

  const preview = document.getElementById("recipe-preview");
  const previewUrl = host.getAttribute("data-preview-url");
  const schedule =
    preview && previewUrl ? makePreview(preview, previewUrl) : null;

  const dark = isDark();

  /* CodeMirror measures the page as it starts, so show the host first. */
  host.hidden = false;

  const view = new EditorView({
    state: EditorState.create({
      doc: source.value,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        highlightActiveLineGutter(),
        history(),
        bracketMatching(),
        highlightSelectionMatches(),
        cooklang,
        syntaxHighlighting(dark ? DARK_HIGHLIGHT : LIGHT_HIGHLIGHT),
        baseTheme(dark),
        keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap]),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({
          "aria-label": source.getAttribute("data-label") || "Recipe",
        }),
        EditorView.updateListener.of((update) => {
          if (!update.docChanged) return;
          const text = update.state.doc.toString();
          /* The form sends the text area, so it holds the true value. */
          source.value = text;
          if (schedule) schedule(text);
        }),
      ],
    }),
    parent: room,
  });

  /* The text area is now the carrier of the value, not the control. */
  source.hidden = true;
  source.setAttribute("aria-hidden", "true");
  source.setAttribute("tabindex", "-1");
  const label = document.querySelector('label[for="recipe-source"]');
  if (label) label.removeAttribute("for");

  /* A form can be sent before the last update reaches the text area. */
  const form = source.form;
  if (form) {
    form.addEventListener("submit", () => {
      source.value = view.state.doc.toString();
    });
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", start);
} else {
  start();
}
