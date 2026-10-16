#!/usr/bin/env node
"use strict";

/**
 * Fixture: a plain tools server exposing the shared tool set.
 *
 *   node tools_server.cjs --transport stdio [--ready-file PATH] [--root DIR]
 *   node tools_server.cjs --transport http --port 8137 [--path /mcp] [--ready-file PATH] [--root DIR]
 *
 * All tools live in common.cjs so every fixture server answers identically.
 */

const common = require("./common.cjs");

common.setLogPrefix("tools-server");

const cfg = common.parseArgs(process.argv.slice(2));
if (cfg.help) {
  common.log(common.USAGE);
  process.exit(0);
}

/** A fresh server per session: a Protocol instance binds to exactly one transport. */
function createServer() {
  const server = new common.McpServer({ name: "aish-fixture-tools", version: "1.0.0" });
  common.registerSharedTools(server, cfg.root);
  return server;
}

async function main() {
  common.log(`transport=${cfg.transport} root=${cfg.root}`);
  if (cfg.transport === "stdio") {
    await common.startStdioServer(createServer(), cfg);
    return;
  }
  await common.startHttpServer(createServer, cfg);
}

main().catch((error) => {
  common.log(`fatal: ${error && error.stack ? error.stack : error}`);
  process.exit(1);
});
