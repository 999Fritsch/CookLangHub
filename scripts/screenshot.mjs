// Take pictures of the running application, so a change can be looked at.
//
// The tests say whether the application behaves. They cannot say whether a
// page reads well, whether a card is crowded, or whether a colour lands.
// This takes the pictures that answer that, at the sizes and palettes that
// people actually use.
//
//   npm run shot                      every page, both palettes, both sizes
//   npm run shot -- --only recipe     one page
//   npm run shot -- --session TOKEN   as a signed-in person
//
// The application must already be running. Start it with
// `docker compose up --build -d` first.
//
// Node and this browser are development tools. The runtime is still the
// Rust binary plus the files in `static/`.

import { mkdir, rm } from "node:fs/promises";
import { chromium } from "playwright";

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at === -1 ? fallback : args[at + 1];
};

const BASE = option("base", "http://localhost:8080");
const OUT = option("out", "screenshots");
const SESSION = option("session", process.env.COOKLANGHUB_SESSION || "");
const ONLY = option("only", "");
const FULL = args.includes("--full-page");

/** The pages worth looking at, and what each one is for. */
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
  { name: "cookbook", path: "/cookbooks/filip/weeknight-dinners", needsSession: false },
  { name: "cookbook-history", path: "/cookbooks/filip/weeknight-dinners/history", needsSession: false },
  { name: "cookbook-sharing", path: "/cookbooks/filip/weeknight-dinners/sharing", needsSession: true },
  { name: "cookbook-add", path: "/cookbooks/filip/weeknight-dinners/recipes", needsSession: true },
];

const SIZES = [
  { name: "desktop", width: 1280, height: 900 },
  // 768 is where the wide navigation appears. It is the tightest that row
  // ever gets, and neither of the other two sizes shows it.
  { name: "tablet", width: 768, height: 1024 },
  { name: "mobile", width: 375, height: 812 },
];

const PALETTES = ["light", "dark"];

/** Cook mode is not a page. It opens over one. */
const OVERLAYS = [
  {
    name: "cook-mode",
    path: "/recipes/filip/chili-sin-carne",
    needsSession: false,
    async open(page) {
      await page.click("[data-start-cooking]");
      await page.waitForSelector("#cooking-overlay", { timeout: 5000 });
      // Let the cards settle into place before the picture.
      await page.waitForTimeout(400);
    },
  },
];

function cookies(palette, facts, signedIn = true) {
  const host = new URL(BASE).hostname;
  const jar = [
    { name: "cooklanghub_theme", value: palette, domain: host, path: "/" },
    { name: "cooklanghub_fact_colour", value: facts, domain: host, path: "/" },
  ];
  if (SESSION && signedIn) {
    jar.push({ name: "cooklanghub_session", value: SESSION, domain: host, path: "/" });
  }
  return jar;
}

async function shoot(browser, { name, path, needsSession, open }, size, palette, facts, signedIn = true) {
  if (needsSession && (!SESSION || !signedIn)) return null;

  const context = await browser.newContext({
    viewport: { width: size.width, height: size.height },
    deviceScaleFactor: 1,
  });
  await context.addCookies(cookies(palette, facts, signedIn));
  const page = await context.newPage();

  const problems = [];
  page.on("console", (message) => {
    if (message.type() === "error") problems.push(message.text().slice(0, 200));
  });
  page.on("pageerror", (error) => problems.push(String(error).slice(0, 200)));

  try {
    const response = await page.goto(`${BASE}${path}`, {
      waitUntil: "networkidle",
      timeout: 20000,
    });
    if (open) await open(page);

    // A page that scrolls sideways is a fault worth catching in the same
    // pass, because a picture of the top of it does not show the fault.
    const overflow = await page.evaluate(() => {
      const de = document.documentElement;
      return de.scrollWidth - de.clientWidth;
    });

    const suffix =
      (facts === "coloured" ? "-coloured" : "") + (signedIn ? "" : "-no-account");
    const file = `${OUT}/${name}-${size.name}-${palette}${suffix}.png`;
    await page.screenshot({ path: file, fullPage: FULL });

    return { file, status: response?.status(), overflow, problems };
  } catch (error) {
    return { file: null, error: String(error).split("\n")[0], problems };
  } finally {
    await context.close();
  }
}

const browser = await chromium.launch();
await rm(OUT, { recursive: true, force: true });
await mkdir(OUT, { recursive: true });

const wanted = [...PAGES, ...OVERLAYS].filter((p) => !ONLY || p.name === ONLY);
const taken = [];

for (const target of wanted) {
  for (const size of SIZES) {
    for (const palette of PALETTES) {
      const shot = await shoot(browser, target, size, palette, "plain");
      if (shot) taken.push({ target: target.name, size: size.name, palette, ...shot });
    }
  }
}

// The fact colours are a choice, so both answers are worth a picture. One
// page and one size is enough to see the difference.
for (const target of wanted.filter((p) => p.name === "preferences" || p.name === "recipe" || p.name === "cookbook")) {
  const shot = await shoot(browser, target, SIZES[0], "light", "coloured");
  if (shot) taken.push({ target: target.name, size: "desktop", palette: "light-coloured", ...shot });
}

// A visitor with no account sees a different navigation, different
// buttons, and only the public Recipes. Nothing else in this tool looks at
// that, so it is easy for a fault there to go unseen.
for (const target of wanted.filter((p) => !p.needsSession)) {
  for (const size of SIZES) {
    const shot = await shoot(browser, target, size, "light", "plain", false);
    if (shot) {
      taken.push({ target: target.name, size: size.name, palette: "light-no-account", ...shot });
    }
  }
}

await browser.close();

let faults = 0;
for (const shot of taken) {
  const bits = [];
  if (shot.error) {
    bits.push(`FAILED ${shot.error}`);
    faults++;
  }
  if (shot.status && shot.status >= 400) {
    bits.push(`status ${shot.status}`);
    faults++;
  }
  if (shot.overflow > 1) {
    bits.push(`SCROLLS SIDEWAYS by ${shot.overflow}px`);
    faults++;
  }
  if (shot.problems?.length) {
    bits.push(`console: ${shot.problems[0]}`);
    faults++;
  }
  console.log(
    `${shot.file ?? `${shot.target}-${shot.size}-${shot.palette}`}${bits.length ? "  <-- " + bits.join("; ") : ""}`,
  );
}

console.log(`\n${taken.length} pictures in ${OUT}/, ${faults} thing(s) to look at`);
if (!SESSION) {
  console.log("No session was given, so the pages that need one were skipped.");
}
