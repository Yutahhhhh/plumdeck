import { chromium } from "playwright";
import type { BrowserContext, Locator, Page } from "playwright";
import type { Config } from "./config.ts";

// Selectors confirmed against gemini.google.com/gem/* (JP locale), 2026-08.
const COMPOSER = [
  'div.ql-editor[contenteditable="true"]',
  'rich-textarea div[contenteditable="true"]',
  '[contenteditable="true"][role="textbox"]',
].map((selector) => `${selector}:visible`).join(", ");

const SEND = [
  'button[aria-label*="送信" i]',
  'button[aria-label*="Send" i]',
  "button.send-button",
].map((selector) => `${selector}:visible`).join(", ");

const STOP = [
  'button[aria-label*="停止" i]',
  'button[aria-label*="Stop" i]',
  'button[aria-label*="回答を停止" i]',
].map((selector) => `${selector}:visible`).join(", ");

const RESPONSE = [
  ".model-response-text",
  "message-content",
  "model-response",
  ".markdown-main-panel",
].join(", ");

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export class GeminiUnavailableError extends Error {
  override name = "GeminiUnavailableError";
}

export class GeminiGem {
  private ctx!: BrowserContext;
  private page!: Page;

  constructor(private cfg: Config) {}

  async open(): Promise<void> {
    const launch = (channel?: string) =>
      chromium.launchPersistentContext(this.cfg.userDataDir, {
        headless: this.cfg.headless,
        channel,
        viewport: { width: 1280, height: 900 },
        locale: "ja-JP",
        args: ["--disable-blink-features=AutomationControlled"],
      });
    try {
      this.ctx = await launch(this.cfg.useChrome ? "chrome" : undefined);
    } catch (err) {
      if (!this.cfg.useChrome) throw err;
      console.log(
        "Chrome を起動できませんでした。同梱 Chromium にフォールバックします。",
        (err as Error).message,
      );
      this.ctx = await launch(undefined);
    }
    this.page = this.ctx.pages()[0] ?? (await this.ctx.newPage());
    this.page.setDefaultTimeout(60_000);
    console.log("Gemini browser launched");
    await this.goToGem();
    console.log("Gemini opened:", this.page.url());
    await this.ensureLoggedIn();
  }

  private async goToGem(): Promise<void> {
    await this.page.goto(this.cfg.gemUrl, { waitUntil: "domcontentloaded", timeout: 30_000 });
  }

  private composer(): Locator {
    return this.page.locator(COMPOSER).first();
  }

  private async ensureLoggedIn(): Promise<void> {
    const timeoutMs = this.cfg.loginOnly ? 15 * 60_000 : 60_000;
    const deadline = Date.now() + timeoutMs;
    console.log("Waiting for Gemini prompt input:", this.page.url());
    while (Date.now() < deadline) {
      if (this.page.isClosed()) {
        throw new GeminiUnavailableError("Gemini browser window was closed; classification stopped.");
      }
      const onLogin = /accounts\.google\.com/.test(this.page.url());
      if (onLogin && !this.cfg.loginOnly) {
        throw new GeminiUnavailableError("Google sign-in is required. Run npm run login before retrying classification.");
      }
      if (this.page.url().includes("/gem/")) {
        try {
          // Let the initial navigation hydrate. Reloading after four seconds
          // interrupted it and could leave a busy renderer with no input.
          await this.composer().waitFor({
            state: "visible", timeout: Math.max(1, Math.min(5_000, deadline - Date.now())),
          });
          if (this.page.url().includes("/gem/")) return;
        } catch {
          if (this.page.isClosed()) {
            throw new GeminiUnavailableError("Gemini browser window was closed; classification stopped.");
          }
        }
      }
      await sleep(Math.max(0, Math.min(500, deadline - Date.now())));
    }
    throw new GeminiUnavailableError(`Gemini prompt input did not become ready within ${timeoutMs / 1000}s; classification stopped.`);
  }

  /** Fresh conversation so each batch is independent and the output stays small. */
  async newChat(): Promise<void> {
    if (this.page.isClosed()) {
      throw new GeminiUnavailableError("Gemini browser window was closed; classification stopped.");
    }
    // The sidebar's global New chat leaves the custom Gem. Reopen the Gem
    // itself so its classification instructions remain in effect each batch.
    await this.goToGem();
    await this.ensureLoggedIn();
    await this.composer().waitFor({ state: "visible", timeout: 30_000 });
    await sleep(500);
  }

  /** Send one prompt, wait for the answer to settle, return its text. */
  async ask(prompt: string, timeoutMs: number): Promise<string> {
    const editor = this.composer();
    await editor.fill("");
    await editor.click();
    await this.page.keyboard.insertText(prompt);
    await sleep(400);

    const typed = (await editor.innerText().catch(() => "")).length;
    if (typed < Math.min(200, prompt.length / 2)) {
      throw new Error(`composer did not accept the prompt (got ${typed} chars)`);
    }

    const send = this.page.locator(SEND).first();
    await send.waitFor({ state: "visible", timeout: 15_000 });
    for (let i = 0; i < 24 && !(await send.isEnabled().catch(() => true)); i++) {
      await sleep(250);
    }
    const responses = this.page.locator(RESPONSE);
    const previousCount = await responses.count();
    const previousText = await responses.last().innerText({ timeout: 1000 }).catch(() => "");
    await send.click();

    return this.waitForAnswer(timeoutMs, previousCount, previousText);
  }

  private async waitForAnswer(timeoutMs: number, previousCount: number, previousText: string): Promise<string> {
    if (this.page.isClosed()) {
      throw new GeminiUnavailableError("Gemini browser window was closed; classification stopped.");
    }
    const responses = this.page.locator(RESPONSE);
    const stop = this.page.locator(STOP).first();
    const start = Date.now();

    // Generation started: a stop control shows up (best effort).
    for (let i = 0; i < 12; i++) {
      if (await stop.isVisible().catch(() => false)) break;
      await sleep(500);
    }
    console.log("Gemini response elements:", JSON.stringify({
      responses: await responses.count(),
      modelResponseText: await this.page.locator(".model-response-text").count(),
      messageContent: await this.page.locator("message-content").count(),
      modelResponse: await this.page.locator("model-response").count(),
      markdownPanel: await this.page.locator(".markdown-main-panel").count(),
    }));

    const stableFor = 4_000;
    let text = "";
    let fresh = false;
    let lastChange = Date.now();

    while (Date.now() - start < timeoutMs) {
      if (this.page.isClosed()) {
        throw new GeminiUnavailableError("Gemini browser window was closed; classification stopped.");
      }
      await sleep(1_000);
      const now = await responses
        .last()
        .innerText({ timeout: 1000 })
        .catch(() => "");
      fresh = fresh || (now.trim().length > 0 &&
        (now !== previousText || await responses.count() > previousCount));
      if (now !== text) {
        text = now;
        lastChange = Date.now();
        continue;
      }
      const generating = await stop.isVisible().catch(() => false);
      if (fresh && !generating && text.trim().length > 0 && Date.now() - lastChange >= stableFor) {
        return text;
      }
    }
    if (fresh && text.trim()) return text; // best effort on timeout
    if (await stop.isVisible().catch(() => false)) {
      await stop.click({ timeout: 5_000 }).catch(() => {});
      await stop.waitFor({ state: "hidden", timeout: 10_000 }).catch(() => {});
    }
    throw new Error(`response timed out after ${timeoutMs}ms`);
  }

  async close(): Promise<void> {
    await this.ctx?.close().catch(() => {});
  }
}
