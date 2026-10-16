#!/usr/bin/env node
"use strict";

/**
 * Fixture: the tools server plus the two server-initiated behaviours the aish client
 * needs to survive — a late tool registration advertised with a real
 * `notifications/tools/list_changed`, and a tool that sends real `roots/list` and
 * `sampling/createMessage` requests back to the client.
 *
 *   node features_server.cjs --transport stdio [--ready-file PATH] [--root DIR]
 *   node features_server.cjs --transport http --port 8138 [--ready-file PATH] [--root DIR]
 */

const fs = require("fs");
const path = require("path");

const { z } = require("zod");
const {
  CreateMessageResultSchema,
  ListRootsResultSchema,
} = require("@modelcontextprotocol/sdk/types.js");

const common = require("./common.cjs");

common.setLogPrefix("features-server");

const cfg = common.parseArgs(process.argv.slice(2));
if (cfg.help) {
  common.log(common.USAGE);
  process.exit(0);
}

/** How long `ask_client` waits for a client answer before recording a timeout error. */
const ASK_TIMEOUT_MS = Number(process.env.MCP_FIXTURE_ASK_TIMEOUT || 10000);
/** Delay before the post-response tool registration, so the client sees the call result first. */
const LIST_CHANGED_DELAY_MS = 50;

/**
 * Sends a request to the client and captures the outcome as either
 * `{result: <raw result>}` or `{error: {code, message}}`, which is exactly how the
 * matrix driver reads `ask-client.json` (`data["roots"]["result"]`,
 * `data["sampling"]["error"]["code"]`).
 */
async function capture(send) {
  try {
    return { result: await send() };
  } catch (error) {
    if (error && typeof error.code === "number") {
      return { error: { code: error.code, message: String(error.message) } };
    }
    return { error: { code: null, message: String((error && error.message) || error) } };
  }
}

function describeOutcome(label, outcome, describeResult) {
  if (outcome.error) {
    return `${label}: error code=${outcome.error.code} message=${outcome.error.message}`;
  }
  return `${label}: ${describeResult(outcome.result)}`;
}

function truncate(value, max) {
  const text = String(value);
  return text.length <= max ? text : `${text.slice(0, max)}…(${text.length} chars)`;
}

function describeRoots(result) {
  const roots = Array.isArray(result && result.roots) ? result.roots : [];
  const uris = roots.map((root) => root && root.uri).join(", ");
  return `ok ${roots.length} root(s)${uris ? ` [${uris}]` : ""}`;
}

function describeSampling(result) {
  const blocks = Array.isArray(result && result.content)
    ? result.content
    : result && result.content
      ? [result.content]
      : [];
  const text = blocks
    .map((block) => (block && block.type === "text" ? block.text : `<${(block && block.type) || "unknown"}>`))
    .join(" ");
  return `ok model=${(result && result.model) || "?"} text="${truncate(text, 120)}"`;
}

function createServer() {
  const server = new common.McpServer({ name: "aish-fixture-features", version: "1.0.0" });
  common.registerSharedTools(server, cfg.root);

  /** Guards against registering `added_after_change` twice. */
  let lateToolRegistered = false;

  server.registerTool(
    "list_added",
    {
      description:
        "Returns 'added', then registers the tool 'added_after_change' and emits " +
        "notifications/tools/list_changed to the client.",
      inputSchema: {},
    },
    async () => {
      if (!lateToolRegistered) {
        lateToolRegistered = true;
        // Fire after the current tools/call has been answered, so the notification
        // cannot race ahead of the result the client is waiting for.
        setTimeout(() => {
          try {
            // Note: `registerTool` on a connected McpServer already emits
            // notifications/tools/list_changed itself, so the client may observe two
            // identical notifications. We send an explicit one anyway so the fixture
            // keeps emitting the required notification regardless of SDK internals;
            // duplicate list_changed notifications are legal and idempotent.
            server.registerTool(
              "added_after_change",
              { description: "Registered after notifications/tools/list_changed was emitted.", inputSchema: {} },
              async () => common.textResult("added after change"),
            );
            server.sendToolListChanged();
            common.log("registered added_after_change and emitted notifications/tools/list_changed");
          } catch (error) {
            lateToolRegistered = false;
            common.log(`failed to add late tool: ${error && error.message}`);
          }
        }, LIST_CHANGED_DELAY_MS);
      }
      return common.textResult("added");
    },
  );

  server.registerTool(
    "ask_client",
    {
      description:
        "Sends a real roots/list request and a real sampling/createMessage request to the " +
        "client, writes ask-client.json below --root and returns a summary.",
      inputSchema: {},
    },
    async (_args, extra) => {
      // extra.sendRequest routes the outgoing request on the stream that belongs to the
      // client's current request, which is what the Streamable HTTP transport needs;
      // server.server.request() sends on the standalone SSE stream instead.
      const send = (request, resultSchema) => {
        const options = { timeout: ASK_TIMEOUT_MS };
        if (extra && typeof extra.sendRequest === "function") {
          return extra.sendRequest(request, resultSchema, options);
        }
        return server.server.request(request, resultSchema, options);
      };

      const roots = await capture(() => send({ method: "roots/list" }, ListRootsResultSchema));
      const sampling = await capture(() =>
        send(
          {
            method: "sampling/createMessage",
            params: {
              messages: [
                {
                  role: "user",
                  content: { type: "text", text: "Reply with the single word: pong." },
                },
              ],
              maxTokens: 32,
            },
          },
          CreateMessageResultSchema,
        ),
      );

      const payload = { roots, sampling };
      const file = path.join(cfg.root, "ask-client.json");
      fs.writeFileSync(file, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
      common.log(`wrote ${file}`);

      const summary = [
        describeOutcome("roots", roots, describeRoots),
        describeOutcome("sampling", sampling, describeSampling),
      ].join("; ");
      return common.textResult(summary);
    },
  );

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
