import { chromium } from "playwright";
const [app, forge, user, pass] = process.argv.slice(2);
const b = await chromium.launch();
const c = await b.newContext({ viewport: { width: 1280, height: 950 }, ignoreHTTPSErrors: false });
const p = await c.newPage();
const errs = []; p.on("console", m => m.type()==="error" && errs.push(m.text()));
const trail = [];
p.on("response", r => { if ([301,302,303,307].includes(r.status()) || r.url().includes("/auth/")) trail.push(`${r.status()} ${r.url().slice(0,110)}`); });

await p.goto(app, { waitUntil: "domcontentloaded" });
await p.getByRole("link", { name: /^Sign in$/ }).first().click();
await p.waitForLoadState("domcontentloaded"); await p.waitForTimeout(1200);

// Forgejo's own login form
if (p.url().includes("/user/login")) {
  await p.fill('input[name="user_name"]', user);
  await p.fill('input[name="password"]', pass);
  await p.locator('form button.ui.primary.button, form button[type=submit]').first().click();
  await p.waitForLoadState("domcontentloaded"); await p.waitForTimeout(1200);
}
// the grant page, when Forgejo shows one
const grant = p.getByRole("button", { name: /Authorize|Grant|Zulassen/i }).first();
if (await grant.count()) { await grant.click(); await p.waitForLoadState("domcontentloaded"); await p.waitForTimeout(1200); }

await p.waitForTimeout(1200);
const cookies = await c.cookies();
const session = cookies.find(x => x.name === "cooklanghub_session");
const body = await p.locator("body").innerText();
console.log(JSON.stringify({
  endedAt: p.url(),
  signedIn: /Sign out/.test(body),
  whoami: (body.match(/Sign out/) ? (body.split("\n").slice(0,12).join(" | ")) : body.slice(0,200)).slice(0,180),
  cookie: session ? { secure: session.secure, httpOnly: session.httpOnly, sameSite: session.sameSite, value: session.value.slice(0,10)+"…" } : null,
  redirects: trail.slice(-6), errs
}, null, 1));
if (session) console.log("SESSION=" + session.value);
await b.close();
