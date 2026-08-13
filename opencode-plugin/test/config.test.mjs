import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";

// resolveConfig reads process.env at call time; the SDK import is dynamic so
// we can set env before the module loads (node --test runs each file in its
// own process, so this does not leak into other test files).
process.env.PC_BIN = "/custom/pc";
process.env.PC_PROVIDERS = "demo, telegram";
process.env.PC_TELEGRAM_TOKEN = "tok-123";
process.env.PC_TELEGRAM_CONFIG = '{"base_url":"https://local.example"}';
process.env.PC_DISCORD_TOKEN = "discord-tok";

const { resolveConfig, childEnv, defaultStateFile } = await import("../dist/config.js");

test("env fallbacks: providers, tokens, pcBin", () => {
  const config = resolveConfig({});
  assert.equal(config.pcBin, "/custom/pc");
  assert.deepEqual(config.providers, ["demo", "telegram"]);
  assert.equal(config.tokens.telegram, "tok-123");
  // Tokens are only merged for configured providers; discord is not loaded.
  assert.equal(config.tokens.discord, undefined);
  assert.deepEqual(config.providerConfig.telegram, { base_url: "https://local.example" });
  assert.equal(config.awaitReply, false);
  assert.equal(config.sessionPrefix, undefined);
});

test("options override env", () => {
  const config = resolveConfig({
    pcBin: "/elsewhere/pc",
    providers: ["discord"],
    tokens: { telegram: "override" },
    rooms: { telegram: ["123"] },
    agent: "coder",
    awaitReply: true,
    sessionPrefix: "MSG ",
    ignoreSenderIds: ["bot"],
  });
  assert.equal(config.pcBin, "/elsewhere/pc");
  assert.deepEqual(config.providers, ["discord"]);
  assert.equal(config.tokens.telegram, "override");
  assert.equal(config.tokens.discord, "discord-tok"); // env still merged for non-option providers
  assert.deepEqual(config.rooms, { telegram: ["123"] });
  assert.equal(config.agent, "coder");
  assert.equal(config.awaitReply, true);
  assert.equal(config.sessionPrefix, "MSG ");
  assert.deepEqual([...config.ignoreSenderIds], ["bot"]);
});

test("childEnv sets PC_PROVIDERS and per-provider tokens, strips PC_CONFIG", () => {
  process.env.PC_CONFIG = "/host/pc-config.json";
  const config = resolveConfig({});
  const env = childEnv(config);
  assert.equal(env.PC_CONFIG, undefined);
  assert.equal(env.PC_PROVIDERS, "demo,telegram");
  assert.equal(env.PC_TELEGRAM_TOKEN, "tok-123");
  assert.equal(env.PC_DISCORD_TOKEN, "discord-tok");
  assert.equal(env.PC_TELEGRAM_CONFIG, '{"base_url":"https://local.example"}');
  delete process.env.PC_CONFIG;
});

test("pcConfigFile switches to -c and strips PC_* env from the child", () => {
  process.env.PC_PROVIDERS = "demo";
  process.env.PC_TELEGRAM_TOKEN = "tok-123";
  const config = resolveConfig({ pcConfigFile: "/tmp/pc.json", providers: ["telegram"] });
  assert.equal(config.pcConfigFile, "/tmp/pc.json");
  const env = childEnv(config);
  assert.equal(env.PC_PROVIDERS, undefined);
  assert.equal(env.PC_TELEGRAM_TOKEN, undefined);
  delete process.env.PC_PROVIDERS;
  delete process.env.PC_TELEGRAM_TOKEN;
});

test("default state file uses XDG_STATE_HOME when set", () => {
  const old = process.env.XDG_STATE_HOME;
  process.env.XDG_STATE_HOME = "/tmp/xdg";
  try {
    const file = defaultStateFile();
    assert.equal(path.dirname(file), "/tmp/xdg/opencode/provider-connect");
  } finally {
    if (old === undefined) delete process.env.XDG_STATE_HOME;
    else process.env.XDG_STATE_HOME = old;
  }
});
