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
 *
 * A fifth thing is not CookCLI's at all: the draft. CookCLI edits a file on
 * the machine it runs on, and this application does not. The draft lives in
 * Forgejo, so this file posts the text there while a person writes, and it
 * writes nothing at all into the browser: no key-value store, no database
 * in the page, nothing that survives the tab. A person can close the tab
 * and carry on somewhere else, and that only works because the browser
 * holds none of the work.
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

/*
 * How long to wait after the last keystroke before the draft is saved.
 *
 * Longer than the preview, because a save writes to Forgejo and a preview
 * only renders text. A person who stops to think for a moment has their
 * work saved; a person in the middle of a word does not pay for it.
 */
const DRAFT_DELAY_MS = 1500;

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

/* How many times a save that did not arrive is tried again. */
const DRAFT_ATTEMPTS = 5;

/*
 * Save the draft in Forgejo while the person writes.
 *
 * The draft Version travels both ways. The page opens on one, each answer
 * carries the next, and the next save sends it back. When the stored draft
 * no longer holds the one that was sent, the application refuses the save
 * and says why, and this stops asking. It never replaces the text on the
 * page, because that text is the work of the person looking at it.
 *
 * Nothing is written to the browser. The draft is in Forgejo, so a tab that
 * closes takes nothing with it.
 */
function makeDraft(config) {
  const { url, baseVersion, versionField, status, problem, problemText } =
    config;

  let timer = null;
  let saving = false;
  /* The newest text, whether or not it reached Forgejo. */
  let latest = null;
  /* The last text the application accepted. */
  let saved = null;
  /* A refusal ends the saving. Nothing after it may overwrite anybody. */
  let stopped = false;
  let failures = 0;

  function say(words) {
    if (status) status.textContent = words || "";
  }

  function refuse(words) {
    stopped = true;
    say("");
    if (problemText) problemText.textContent = words || "";
    if (problem) problem.hidden = false;
  }

  function version() {
    return versionField ? versionField.value : "";
  }

  async function send(text) {
    if (stopped) return;
    saving = true;

    try {
      const response = await fetch(url, {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({
          source: text,
          base_version: baseVersion,
          draft_version: version(),
        }).toString(),
      });

      const answer = await response.json().catch(() => null);
      const words = answer && answer.message ? answer.message : "";

      if (response.ok) {
        failures = 0;
        saved = text;
        if (answer && answer.version && versionField) {
          versionField.value = answer.version;
        }
        say(words);
      } else if (response.status === 409) {
        /* Somebody wrote first. The application refused this save, and so
           does this: the text on the page stays exactly as it is. */
        refuse(words);
      } else {
        say(words);
        retry(text);
      }
    } catch (error) {
      /* The answer never arrived. Nothing is known about what happened, so
         the same text goes again. */
      retry(text);
    } finally {
      saving = false;
      /* A change that arrived while this one was in flight goes next. The
         text that just went is not sent again here: a save that did not
         arrive is tried again by `retry`, and only a set number of times,
         so a Forgejo that is down cannot be asked forever. */
      if (latest !== null && latest !== text && latest !== saved) {
        schedule(latest);
      }
    }
  }

  function retry(text) {
    failures += 1;
    if (failures > DRAFT_ATTEMPTS) return;
    window.setTimeout(() => {
      /* Only while this is still the newest text. The person can write more
         while this waits, and sending an older text after a newer one would
         undo them. The check is made here and not above, because what is
         newest can change during the wait. */
      if (latest === text) schedule(text);
    }, DRAFT_DELAY_MS);
  }

  function schedule(text) {
    latest = text;
    if (stopped || saving || text === saved) return;
    if (timer !== null) window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      timer = null;
      send(text);
    }, DRAFT_DELAY_MS);
  }

  /* Save now rather than after the wait. A tab that is going away has no
     time left to wait in. */
  function flush() {
    if (timer !== null) {
      window.clearTimeout(timer);
      timer = null;
    }
    if (stopped || saving || latest === null || latest === saved) return;
    send(latest);
  }

  return { schedule, flush };
}

function start() {
  const host = document.getElementById("recipe-editor");
  const source = document.getElementById("recipe-source");
  if (!source) return;

  const preview = document.getElementById("recipe-preview");
  const previewUrl = host && host.getAttribute("data-preview-url");
  const schedulePreview =
    preview && previewUrl ? makePreview(preview, previewUrl) : null;

  /*
   * The draft. It is set up before CodeMirror and apart from it, so that a
   * browser which keeps the plain text area still saves what a person
   * writes.
   */
  const versionField = document.getElementById("draft-version");
  const baseField = source.form
    ? source.form.querySelector('input[name="base_version"]')
    : null;
  const draftUrl = host && host.getAttribute("data-draft-url");
  const draft =
    draftUrl && baseField && baseField.value
      ? makeDraft({
          url: draftUrl,
          baseVersion: baseField.value,
          versionField,
          status: document.getElementById("draft-status"),
          problem: document.getElementById("draft-problem"),
          problemText: document.getElementById("draft-problem-text"),
        })
      : null;

  /* One place that hears about a change, whatever made it. */
  function changed(text) {
    if (schedulePreview) schedulePreview(text);
    if (draft) draft.schedule(text);
  }

  /* The plain text area on its own. CodeMirror writes the value rather than
     typing into it, so it reports its own changes below. */
  source.addEventListener("input", () => changed(source.value));

  if (draft) {
    /* A person who leaves the tab must not lose the last few seconds. */
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "hidden") draft.flush();
    });
  }

  /*
   * CodeMirror carries its own CSS and puts it on the page as it starts.
   * The Content Security Policy of this application is `default-src 'self'`,
   * which stops a style that arrives inside the page. A shadow root gets
   * the same CSS as an adopted stylesheet instead, and the policy allows
   * that, so the editor is styled and the policy stays as it is.
   *
   * A browser that cannot make a shadow root keeps the plain text area,
   * which works on its own and now saves on its own too.
   */
  if (!host || typeof host.attachShadow !== "function") return;
  const room = host.shadowRoot || host.attachShadow({ mode: "open" });

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
          changed(text);
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
