"use strict";

/**
 * Shared plumbing for the Node MCP fixture servers.
 *
 * These servers exist to exercise the aish MCP client against real SDK servers over
 * both the stdio and the Streamable HTTP transport. Every server in this directory is
 * runnable on its own (`node <server>.cjs [--transport stdio|http] ...`) and depends
 * only on the packages reachable through the `node_modules` symlink sitting next to it
 * (`@modelcontextprotocol/sdk`, `express`, `cors`, `zod`).
 *
 * Contract implemented here (the test driver depends on it):
 *   --transport stdio (default) | http
 *   --port N            HTTP listen port (0 = ephemeral)
 *   --path /mcp         HTTP endpoint path
 *   --ready-file PATH   append `READY <transport> <url-or-pid>` once serving
 *   --root DIR          directory for server state ($MCP_FIXTURE_ROOT, else a temp dir)
 *   --host ADDR         HTTP bind address (default 127.0.0.1 / $MCP_FIXTURE_HOST)
 *
 * Stdout carries protocol bytes only on stdio; every diagnostic goes to stderr.
 */

const crypto = require("crypto");
const fs = require("fs");
const os = require("os");
const path = require("path");

const { z } = require("zod");
const { McpServer } = require("@modelcontextprotocol/sdk/server/mcp.js");
const { StdioServerTransport } = require("@modelcontextprotocol/sdk/server/stdio.js");
const {
  StreamableHTTPServerTransport,
} = require("@modelcontextprotocol/sdk/server/streamableHttp.js");
const { isInitializeRequest } = require("@modelcontextprotocol/sdk/types.js");

const USAGE =
  "usage: node <server>.cjs [--transport stdio|http] [--port N] [--path /mcp] " +
  "[--ready-file PATH] [--root DIR] [--host ADDR]";

/**
 * 1x1 RGBA PNG (opaque #336699). Generated and verified with an independent decoder
 * (PIL: format PNG, mode RGBA, pixel (51, 102, 153, 255)); all chunk CRCs check out.
 */
const PNG_1PX_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mMwTpv5HwAENAIyhHMY8AAAAABJRU5ErkJggg==";

/** Upper bound for `slow` so a bad argument cannot wedge a fixture run forever. */
const MAX_SLEEP_MS = 10 * 60 * 1000;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

let logPrefix = "mcp";

function setLogPrefix(prefix) {
  logPrefix = prefix;
}

/** Every diagnostic goes to stderr: stdout is reserved for protocol bytes on stdio. */
function log(message) {
  process.stderr.write(`[${logPrefix}] ${message}\n`);
}

function fail(message) {
  log(`error: ${message}`);
  process.exit(2);
}

// ---------------------------------------------------------------------------
// CLI, root dir and ready file
// ---------------------------------------------------------------------------

function resolveRoot(requested) {
  const root = requested || process.env.MCP_FIXTURE_ROOT;
  const resolved = path.resolve(
    root || fs.mkdtempSync(path.join(os.tmpdir(), "mcp-fixture-node-")),
  );
  fs.mkdirSync(resolved, { recursive: true });
  return resolved;
}

function parseArgs(argv) {
  const cfg = {
    transport: "stdio",
    transportGiven: false,
    port: 0,
    portGiven: false,
    path: "/mcp",
    readyFile: null,
    root: null,
    token: null,
    host: null,
    help: false,
  };

  const value = (flag, index) => {
    const next = argv[index + 1];
    if (next === undefined) fail(`missing value for ${flag}`);
    return next;
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--transport") {
      cfg.transport = value(arg, i);
      cfg.transportGiven = true;
      i += 1;
    } else if (arg === "--port") {
      cfg.port = Number(value(arg, i));
      cfg.portGiven = true;
      i += 1;
    } else if (arg === "--path") {
      cfg.path = value(arg, i);
      i += 1;
    } else if (arg === "--ready-file") {
      cfg.readyFile = value(arg, i);
      i += 1;
    } else if (arg === "--root") {
      cfg.root = value(arg, i);
      i += 1;
    } else if (arg === "--token") {
      cfg.token = value(arg, i);
      i += 1;
    } else if (arg === "--host") {
      cfg.host = value(arg, i);
      i += 1;
    } else if (arg === "-h" || arg === "--help") {
      cfg.help = true;
    } else if (arg === "stdio" || arg === "http") {
      // Tolerate the `node server.cjs http --port N` spelling.
      cfg.transport = arg;
      cfg.transportGiven = true;
    } else {
      fail(`unknown argument: ${arg}\n${USAGE}`);
    }
  }

  if (cfg.transport !== "stdio" && cfg.transport !== "http") {
    fail(`unsupported --transport '${cfg.transport}' (expected stdio or http)`);
  }
  if (cfg.portGiven && (!Number.isInteger(cfg.port) || cfg.port < 0 || cfg.port > 65535)) {
    fail(`invalid --port '${argv[argv.indexOf("--port") + 1]}'`);
  }
  if (!cfg.path.startsWith("/")) cfg.path = `/${cfg.path}`;
  cfg.root = resolveRoot(cfg.root);
  return cfg;
}

function writeReadyFile(readyFile, transport, reference) {
  if (!readyFile) return;
  const line = `READY ${transport} ${reference}\n`;
  // appendFileSync opens, writes, flushes and closes: the line is on disk before we return.
  fs.appendFileSync(readyFile, line);
  log(`ready file updated: ${line.trim()}`);
}

// ---------------------------------------------------------------------------
// Shared tool set
// ---------------------------------------------------------------------------

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function textResult(text) {
  return { content: [{ type: "text", text }] };
}

function errorResult(text) {
  return { isError: true, content: [{ type: "text", text }] };
}

/**
 * Registers the tool set shared by every fixture server.
 *
 * @param {McpServer} server server to register on
 * @param {string} root directory `write_note` writes under
 */
function registerSharedTools(server, root) {
  const readOnlyIdempotent = { readOnlyHint: true, idempotentHint: true };

  server.registerTool(
    "echo",
    {
      description: "Echoes the provided text back verbatim.",
      inputSchema: { text: z.string().describe("text to echo") },
      annotations: readOnlyIdempotent,
    },
    async ({ text }) => textResult(text),
  );

  server.registerTool(
    "add",
    {
      description: "Adds two numbers and returns the sum as text.",
      inputSchema: { a: z.number().describe("first addend"), b: z.number().describe("second addend") },
      annotations: readOnlyIdempotent,
    },
    async ({ a, b }) => textResult(String(a + b)),
  );

  server.registerTool(
    "slow",
    {
      description: "Sleeps for the requested number of milliseconds, then reports how long it slept.",
      inputSchema: { ms: z.number().describe("milliseconds to sleep") },
      annotations: { readOnlyHint: true },
    },
    async ({ ms }) => {
      if (!Number.isFinite(ms) || ms < 0) return errorResult(`invalid ms: ${ms}`);
      const waited = Math.min(ms, MAX_SLEEP_MS);
      await delay(waited);
      return textResult(`slept ${waited}ms`);
    },
  );

  server.registerTool(
    "fail",
    {
      description: "Always fails at the tool level (isError: true).",
      inputSchema: {},
    },
    async () => errorResult("fixture failure: nothing to do"),
  );

  server.registerTool(
    "big",
    {
      description: "Returns the requested number of kilobytes of text (default 256 KB).",
      inputSchema: { kb: z.number().optional().describe("size in kilobytes, default 256") },
      annotations: { readOnlyHint: true },
    },
    async ({ kb }) => {
      const requested = kb === undefined ? 256 : kb;
      if (!Number.isFinite(requested) || requested < 0) return errorResult(`invalid kb: ${kb}`);
      const bytes = Math.min(Math.floor(requested), 10240) * 1024;
      const pattern = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n";
      const text = pattern.repeat(Math.ceil(bytes / pattern.length)).slice(0, bytes);
      return textResult(text);
    },
  );

  server.registerTool(
    "image",
    {
      description: "Returns a single image content block (a 1x1 PNG) plus a short text block.",
      inputSchema: {},
    },
    async () => ({
      content: [
        { type: "image", data: PNG_1PX_BASE64, mimeType: "image/png" },
        { type: "text", text: "1x1 png image (opaque #336699)" },
      ],
    }),
  );

  server.registerTool(
    "structured",
    {
      description: "Returns structured content ({ok, count}) plus a short text block.",
      inputSchema: {},
      outputSchema: { ok: z.boolean(), count: z.number() },
    },
    async () => ({
      content: [{ type: "text", text: "structured result: ok=true count=3" }],
      structuredContent: { ok: true, count: 3 },
    }),
  );

  server.registerTool(
    "unicode",
    {
      description: "Returns the provided text verbatim (defaults to a non-ASCII string).",
      inputSchema: { text: z.string().optional().describe("text to return, default 你好🙂") },
    },
    async ({ text }) => textResult(text === undefined ? "你好🙂" : text),
  );

  server.registerTool(
    "write_note",
    {
      description: "Writes a note file below --root and returns the absolute path it wrote.",
      inputSchema: { path: z.string().describe("file path relative to --root"), content: z.string() },
      annotations: { destructiveHint: true },
    },
    async ({ path: notePath, content }) => {
      const rootAbs = path.resolve(root);
      const target = path.resolve(rootAbs, notePath);
      if (target !== rootAbs && !target.startsWith(rootAbs + path.sep)) {
        return errorResult(`refusing to write outside --root: ${notePath}`);
      }
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, content, "utf8");
      log(`wrote note ${target} (${Buffer.byteLength(content, "utf8")} bytes)`);
      return textResult(target);
    },
  );

  server.registerTool(
    "delete_all",
    {
      description: "Pretends to delete every note under --root; never touches the filesystem.",
      inputSchema: {},
      annotations: { destructiveHint: true, openWorldHint: true },
    },
    async () => textResult("would delete everything"),
  );
}

// ---------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------

/**
 * Runs `server` on stdio until stdin closes.
 *
 * @param {McpServer} server MCP server to connect
 * @param {object} cfg parsed CLI config
 */
async function startStdioServer(server, cfg) {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  log("stdio transport connected");
  writeReadyFile(cfg.readyFile, "stdio", String(process.pid));

  const bye = () => {
    log("stdin closed, exiting");
    process.exit(0);
  };
  process.stdin.on("end", bye);
  process.stdin.on("close", bye);
}

function headerValue(value) {
  if (Array.isArray(value)) return value[0];
  return typeof value === "string" ? value : undefined;
}

function sendJsonRpcError(res, status, code, message) {
  if (res.headersSent) {
    res.end();
    return;
  }
  res.status(status).json({ jsonrpc: "2.0", error: { code, message }, id: null });
}

/**
 * Runs a stateful Streamable HTTP endpoint for the given server factory.
 *
 * @param {() => McpServer} makeServer fresh server per session (a Protocol instance
 *   cannot be connected to two transports)
 * @param {object} cfg parsed CLI config
 * @param {{authorize?: import("express").RequestHandler}} [options]
 */
async function startHttpServer(makeServer, cfg, options = {}) {
  const express = require("express");
  const cors = require("cors");

  const app = express();
  /** @type {Map<string, {transport: StreamableHTTPServerTransport, server: McpServer}>} */
  const sessions = new Map();

  // One line per request on stderr: the driver uses these to prove session-id echo
  // and that the closing DELETE actually reached the server.
  app.use((req, res, next) => {
    const inbound = headerValue(req.headers["mcp-session-id"]) || "-";
    let logged = false;
    const record = () => {
      if (logged) return;
      logged = true;
      const outbound = headerValue(res.getHeader("mcp-session-id")) || inbound;
      log(`HTTP ${req.method} ${req.originalUrl} -> ${res.statusCode} session_id=${outbound}`);
    };
    // `finish` covers completed responses, `close` also covers aborted ones (a client
    // dropping an SSE stream never emits `finish`).
    res.on("finish", record);
    res.on("close", record);
    next();
  });

  app.use(
    cors({
      origin: "*",
      exposedHeaders: ["Mcp-Session-Id"],
      allowedHeaders: [
        "Content-Type",
        "Accept",
        "Authorization",
        "Mcp-Session-Id",
        "Mcp-Protocol-Version",
        "Last-Event-ID",
      ],
      methods: ["GET", "POST", "DELETE", "OPTIONS"],
    }),
  );

  if (options.authorize) app.use(options.authorize);

  app.use(express.json({ limit: "32mb" }));

  const openSession = async () => {
    const entry = { transport: undefined, server: undefined };
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: () => crypto.randomUUID(),
      onsessioninitialized: (sessionId) => {
        sessions.set(sessionId, entry);
        log(`session initialized: ${sessionId}`);
      },
      onsessionclosed: (sessionId) => {
        sessions.delete(sessionId);
        log(`session closed: ${sessionId}`);
      },
    });
    const sessionServer = makeServer();
    entry.transport = transport;
    entry.server = sessionServer;
    transport.onclose = () => {
      const sessionId = transport.sessionId;
      if (sessionId && sessions.get(sessionId) === entry) {
        sessions.delete(sessionId);
        log(`session transport closed: ${sessionId}`);
      }
    };
    await sessionServer.connect(transport);
    return entry;
  };

  app.post(cfg.path, async (req, res) => {
    try {
      const sessionId = headerValue(req.headers["mcp-session-id"]);
      let entry = sessionId ? sessions.get(sessionId) : undefined;

      if (!entry) {
        if (sessionId) {
          log(`unknown session id on POST: ${sessionId}`);
          sendJsonRpcError(res, 404, -32000, `Session not found: ${sessionId}`);
          return;
        }
        if (!isInitializeRequest(req.body)) {
          log("POST without a session id that is not an initialize request");
          sendJsonRpcError(
            res,
            400,
            -32000,
            "Bad Request: no valid session ID was provided and the request is not an initialize request",
          );
          return;
        }
        entry = await openSession();
      }

      await entry.transport.handleRequest(req, res, req.body);
    } catch (error) {
      log(`POST ${cfg.path} failed: ${error && error.stack ? error.stack : error}`);
      sendJsonRpcError(res, 500, -32603, `Internal error: ${error && error.message}`);
    }
  });

  // GET opens the notification/request stream, DELETE terminates the session.
  const handleSessionRequest = async (req, res) => {
    try {
      const sessionId = headerValue(req.headers["mcp-session-id"]);
      const entry = sessionId ? sessions.get(sessionId) : undefined;
      if (!entry) {
        log(`${req.method} with unknown/missing session id: ${sessionId || "-"}`);
        sendJsonRpcError(
          res,
          sessionId ? 404 : 400,
          -32000,
          sessionId ? `Session not found: ${sessionId}` : "Bad Request: missing Mcp-Session-Id header",
        );
        return;
      }
      await entry.transport.handleRequest(req, res);
    } catch (error) {
      log(`${req.method} ${cfg.path} failed: ${error && error.stack ? error.stack : error}`);
      sendJsonRpcError(res, 500, -32603, `Internal error: ${error && error.message}`);
    }
  };

  app.get(cfg.path, handleSessionRequest);
  app.delete(cfg.path, handleSessionRequest);

  // Body parser / routing errors and anything else that bubbles up.
  app.use((error, req, res, _next) => {
    log(`request error on ${req.method} ${req.originalUrl}: ${error && error.message}`);
    sendJsonRpcError(
      res,
      error && Number.isInteger(error.status) ? error.status : 400,
      -32700,
      `invalid request body: ${error && error.message}`,
    );
  });

  const host = cfg.host || process.env.MCP_FIXTURE_HOST || "127.0.0.1";
  const httpServer = await new Promise((resolve, reject) => {
    const instance = app.listen(cfg.port, host, () => resolve(instance));
    instance.on("error", reject);
  });
  const address = httpServer.address();
  if (!address) {
    // A failed bind (typically EADDRINUSE) can still resolve the listen
    // promise; report it instead of crashing on a null address.
    fail(`could not listen on ${host}:${cfg.port} (is the port already in use?)`);
  }
  const displayHost = host === "0.0.0.0" || host === "::" ? "127.0.0.1" : host;
  const url = `http://${displayHost}:${address.port}${cfg.path}`;
  log(`listening on ${url} (root ${cfg.root})`);
  writeReadyFile(cfg.readyFile, "http", url);

  let shuttingDown = false;
  const shutdown = async (reason) => {
    if (shuttingDown) return;
    shuttingDown = true;
    log(`shutting down (${reason})`);
    httpServer.close();
    for (const entry of sessions.values()) {
      try {
        await entry.transport.close();
        if (entry.server) await entry.server.close();
      } catch (error) {
        log(`error closing session transport: ${error && error.message}`);
      }
    }
    sessions.clear();
    process.exit(0);
  };
  process.on("SIGTERM", () => void shutdown("SIGTERM"));
  process.on("SIGINT", () => void shutdown("SIGINT"));
}

module.exports = {
  McpServer,
  USAGE,
  fail,
  log,
  parseArgs,
  registerSharedTools,
  setLogPrefix,
  startHttpServer,
  startStdioServer,
  textResult,
  errorResult,
  writeReadyFile,
};
