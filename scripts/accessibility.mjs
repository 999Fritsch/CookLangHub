// Check the running application against the accessibility rules.
//
// The tests say whether the application behaves, and the pictures say
// whether it reads well. Neither says whether a control has a name, whether
// a heading is in order, or whether a colour carries meaning on its own.
// axe-core answers that, and this runs it on every page.
//
//   npm run audit                      every page, both palettes
//   npm run audit -- --only recipe     one page
//   npm run audit -- --session TOKEN   as a signed-in person
//   npm run audit -- --engine firefox  chromium, firefox, or webkit
//   npm run audit -- --contrast        also measure the text contrast
//
// The application must already be running. Start it with
// `docker compose up --build -d` first.
//
// The exit code is the number of faults, so a check can fail on it.
//
// **Contrast is measured and never a gate.** `CLAUDE.md` says that
// matching CookCLI matters more than a ratio for this prototype, so the
// contrast rules stay off in a normal run. `--contrast` turns them on and
// prints what it measured as a table. It still adds nothing to the count.
//
// axe-core is in `scripts/vendor/`, so this needs no network. The page it
// looks at has a `default-src 'self'` policy, which stops a script that
// arrives inside the page, so axe goes in ahead of the page instead.
//
// Node and this browser are development tools. The runtime is still the
// Rust binary plus the files in `static/`.

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as playwright from "playwright";

const HERE = dirname(fileURLToPath(import.meta.url));

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at === -1 ? fallback : args[at + 1];
};

const BASE = option("base", "http://localhost:8080");
const OUT = option("out", "");
const SESSION = option("session", process.env.COOKLANGHUB_SESSION || "");
const ONLY = option("only", "");
const ENGINE = option("engine", "chromium");
const WIDTH = Number(option("width", "1280"));
const CONTRAST = args.includes("--contrast");

/**
 * The pages worth checking, and what each one is for.
 *
 * The issue asks for the Recipe page, the editor, and the Suggestion page.
 * The rest cost almost nothing to add, and a fault on any of them is the
 * same kind of fault.
 */
const PAGES = [
  { name: "recipes", path: "/", needsSession: true },
  { name: "explore", path: "/explore", needsSession: false },
  { name: "recipe", path: "/recipes/filip/chili-sin-carne", needsSession: false },
  { name: "recipe-scaled", path: "/recipes/filip/chili-sin-carne?servings=8", needsSession: false },
  { name: "preferences", path: "/preferences", needsSession: false },
  { name: "new-recipe", path: "/recipes/new", needsSession: true },
  { name: "sharing", path: "/recipes/filip/chili-sin-carne/sharing", needsSession: true },
  { name: "discussions", path: "/recipes/filip/chili-sin-carne/discussions", needsSession: false },
  { name: "editor", path: "/recipes/filip/chili-sin-carne/edit", needsSession: true },
  { name: "history", path: "/recipes/filip/chili-sin-carne/history", needsSession: false },
  { name: "cookbooks", path: "/cookbooks", needsSession: true },
  { name: "cookbook-new", path: "/cookbooks/new", needsSession: true },
  { name: "profile", path: "/cooks/filip", needsSession: false },
  { name: "variations", path: "/recipes/filip/chili-sin-carne/variations", needsSession: false },
  { name: "suggestions", path: "/recipes/filip/chili-sin-carne/suggestions", needsSession: false },
  { name: "suggestion", path: "/recipes/filip/chili-sin-carne/suggestions/2", needsSession: false },
  { name: "inbox", path: "/suggestions", needsSession: true },
  { name: "cookbook", path: "/cookbooks/filip/weeknight-dinners", needsSession: false },
  { name: "cookbook-history", path: "/cookbooks/filip/weeknight-dinners/history", needsSession: false },
  { name: "cookbook-sharing", path: "/cookbooks/filip/weeknight-dinners/sharing", needsSession: true },
  { name: "cookbook-add", path: "/cookbooks/filip/weeknight-dinners/recipes", needsSession: true },
];

/** Cook mode is not a page. It opens over one, and it is the dialog. */
const OVERLAYS = [
  {
    name: "cook-mode",
    path: "/recipes/filip/chili-sin-carne",
    needsSession: false,
    async open(page) {
      await page.click("[data-start-cooking]");
      await page.waitForSelector("#cooking-overlay", { timeout: 5000 });
      await page.waitForTimeout(400);
    },
  },
];

const PALETTES = ["light", "dark"];

/**
 * Which rules to run.
 *
 * The WCAG 2.0 and 2.1 rules at level A and AA, and nothing else. The
 * best-practice set of axe holds advice that is not a rule, and mixing the
 * two makes the count mean less.
 */
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

/** Measured, never a gate. See the note at the top of this file. */
const CONTRAST_RULES = ["color-contrast"];

const axeSource = await readFile(join(HERE, "vendor", "axe.min.js"), "utf8");

function cookies(palette, signedIn) {
  const host = new URL(BASE).hostname;
  const jar = [
    { name: "cooklanghub_theme", value: palette, domain: host, path: "/" },
    { name: "cooklanghub_fact_colour", value: "plain", domain: host, path: "/" },
  ];
  if (SESSION && signedIn) {
    jar.push({ name: "cooklanghub_session", value: SESSION, domain: host, path: "/" });
  }
  return jar;
}

async function check(browser, { name, path, needsSession, open }, palette) {
  const signedIn = Boolean(SESSION);
  if (needsSession && !signedIn) return null;

  const context = await browser.newContext({
    viewport: { width: WIDTH, height: 900 },
    deviceScaleFactor: 1,
  });
  await context.addCookies(cookies(palette, signedIn));

  // The policy of the application is `default-src 'self'`, so a script tag
  // that this tool adds to the page is refused. A script that goes in ahead
  // of the page is not part of the page, and it runs.
  await context.addInitScript({ content: axeSource });

  const page = await context.newPage();

  try {
    const response = await page.goto(`${BASE}${path}`, {
      waitUntil: "networkidle",
      timeout: 20000,
    });
    if (response && response.status() >= 400) {
      return { name, palette, error: `status ${response.status()}` };
    }
    if (open) await open(page);

    const result = await page.evaluate(
      async ({ tags, contrastRules, contrast }) => {
        const rules = {};
        for (const rule of contrastRules) rules[rule] = { enabled: contrast };
        return await window.axe.run(document, {
          runOnly: { type: "tag", values: tags },
          rules,
          resultTypes: ["violations"],
        });
      },
      { tags: TAGS, contrastRules: CONTRAST_RULES, contrast: CONTRAST },
    );

    const faults = [];
    const contrasts = [];
    for (const violation of result.violations) {
      for (const node of violation.nodes) {
        const where = node.target.join(" ");
        if (CONTRAST_RULES.includes(violation.id)) {
          const facts = (node.any || []).map((c) => c.data).find(Boolean) || {};
          contrasts.push({
            page: name,
            palette,
            where,
            ratio: facts.contrastRatio,
            wanted: facts.expectedContrastRatio,
            foreground: facts.fgColor,
            background: facts.bgColor,
            size: facts.fontSize,
            weight: facts.fontWeight,
            text: (node.html || "").replace(/\s+/g, " ").slice(0, 70),
          });
          continue;
        }
        faults.push({
          page: name,
          palette,
          rule: violation.id,
          impact: node.impact || violation.impact,
          help: violation.help,
          where,
          html: (node.html || "").replace(/\s+/g, " ").slice(0, 120),
        });
      }
    }
    return { name, palette, faults, contrasts };
  } catch (error) {
    return { name, palette, error: String(error).split("\n")[0] };
  } finally {
    await context.close();
  }
}

const engine = playwright[ENGINE];
if (!engine) {
  console.error(`No engine named ${ENGINE}. Use chromium, firefox, or webkit.`);
  process.exit(2);
}

const browser = await engine.launch();
const wanted = [...PAGES, ...OVERLAYS].filter((p) => !ONLY || p.name === ONLY);

const faults = [];
const contrasts = [];
const broken = [];

for (const target of wanted) {
  for (const palette of PALETTES) {
    const answer = await check(browser, target, palette);
    if (!answer) continue;
    if (answer.error) {
      broken.push(answer);
      console.log(`${answer.name}-${answer.palette}  <-- FAILED ${answer.error}`);
      continue;
    }
    faults.push(...answer.faults);
    contrasts.push(...answer.contrasts);
    const count = answer.faults.length;
    console.log(
      `${answer.name}-${answer.palette}  ${count === 0 ? "clean" : `${count} fault(s)`}`,
    );
  }
}

await browser.close();

if (faults.length) {
  console.log("\nFaults");
  console.log("------");
  for (const fault of faults) {
    console.log(`${fault.page}-${fault.palette}  ${fault.rule}  (${fault.impact})`);
    console.log(`    ${fault.help}`);
    console.log(`    at ${fault.where}`);
    console.log(`    ${fault.html}`);
  }
}

if (CONTRAST) {
  console.log("\nText contrast, measured");
  console.log("-----------------------");
  if (contrasts.length === 0) {
    console.log("Every text and background pair meets the ratio it needs.");
  } else {
    console.log(
      "ratio  wanted  page                palette  foreground  background  text",
    );
    for (const one of contrasts) {
      console.log(
        [
          String(one.ratio).padEnd(6),
          String(one.wanted).padEnd(7),
          one.page.padEnd(19),
          one.palette.padEnd(8),
          String(one.foreground).padEnd(11),
          String(one.background).padEnd(11),
          one.text,
        ].join(" "),
      );
    }
    console.log(
      "\nThis is a measurement and not a fault. `CLAUDE.md` keeps the CookCLI",
    );
    console.log("colours as they are for the prototype.");
  }
}

if (OUT) {
  await mkdir(OUT, { recursive: true });
  await writeFile(
    join(OUT, `accessibility-${ENGINE}.json`),
    JSON.stringify({ engine: ENGINE, base: BASE, faults, contrasts, broken }, null, 2),
  );
  console.log(`\nThe whole answer is in ${OUT}/accessibility-${ENGINE}.json`);
}

const total = faults.length + broken.length;
console.log(`\n${wanted.length} page(s) on ${ENGINE}, ${total} thing(s) to look at`);
if (!SESSION) {
  console.log("No session was given, so the pages that need one were skipped.");
}

// A count and not an exception, so that a check can fail on it and the whole
// answer still reaches the screen first.
process.exitCode = total > 0 ? 1 : 0;
