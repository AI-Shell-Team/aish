#!/usr/bin/env node
"use strict";

/**
 * Fixture: a Streamable HTTP server that requires `Authorization: Bearer <token>` on
 * every request (CORS preflights excepted) and answers 401 with a JSON-RPC error body
 * otherwise. Exposes a single tool so the driver can prove the token check happens
 * before the MCP session is created.
 *
 *   node auth_http_server.cjs --port 8139 [--path /mcp] --token secret \
 *        [--ready-file PATH] [--root DIR]
 *
 * The token comes from --token or $MCP_FIXTURE_TOKEN; there is no stdio mode.
 */

const crypto = require("crypto");

const { z } = require("zod");

const common = require("./common.cjs");

common.setLogPrefix("auth-http-server");

const cfg = common.parseArgs(process.argv.slice(2));
if (cfg.help) {
  common.log(common.USAGE);
  process.exit(0);
}

const token = cfg.token || process.env.MCP_FIXTURE_TOKEN;
if (!token) {
  common.fail("no token configured: pass --token or set $MCP_FIXTURE_TOKEN");
}

/**
 * Rejects any request without the exact `Authorization: Bearer <token>` header.
 * Preflight requests carry no credentials by design, so OPTIONS is passed through to
 * the CORS middleware.
 */
function authorize(token) {
  const expected = Buffer.from(`Bearer ${token}`, "utf8");
  return (req, res, next) => {
    if (req.method === "OPTIONS") {
      next();
      return;
    }
    const provided = req.headers.authorization;
    const ok =
      typeof provided === "string" &&
      provided.length === expected.length &&
      crypto.timingSafeEqual(Buffer.from(provided, "utf8"), expected);
    if (!ok) {
      common.log(
        `401 ${req.method} ${req.originalUrl}: ${provided ? "invalid bearer token" : "missing Authorization header"}`,
      );
      res.setHeader("WWW-Authenticate", 'Bearer realm="mcp-fixture"');
      res.status(401).json({
        jsonrpc: "2.0",
        error: { code: -32001, message: "Unauthorized: missing or invalid bearer token" },
        id: null,
      });
      return;
    }
    next();
  };
}

/** A fresh server per session: a Protocol instance binds to exactly one transport. */
function createServer() {
  const server = new common.McpServer({ name: "aish-fixture-auth-http", version: "1.0.0" });
  server.registerTool(
    "echo",
    {
      description: "Echoes the provided text back verbatim.",
      inputSchema: { text: z.string().describe("text to echo") },
      annotations: { readOnlyHint: true, idempotentHint: true },
    },
    async ({ text }) => common.textResult(text),
  );
  return server;
}

async function main() {
  // The auth fixture is HTTP only: `--transport http` is the default here, and stdio
  // (the shared default) is rejected explicitly.
  if (cfg.transportGiven && cfg.transport !== "http") {
    common.fail("auth_http_server.cjs only supports --transport http");
  }
  cfg.transport = "http";
  common.log(`transport=http root=${cfg.root} token=${token.length} chars`);
  await common.startHttpServer(createServer, cfg, { authorize: authorize(token) });
}

main().catch((error) => {
  common.log(`fatal: ${error && error.stack ? error.stack : error}`);
  process.exit(1);
});
