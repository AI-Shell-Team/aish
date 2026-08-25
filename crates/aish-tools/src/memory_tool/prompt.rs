pub(crate) const DESCRIPTION: &str = "Search, store, list, or forget long-term memories.";

pub(crate) const PROMPT: &str = r#"Use this tool for long-term memory operations.

Usage:
- Use search to recall relevant saved knowledge before acting on user preferences or prior context.
- Use store only for durable facts that are likely useful later. Storing and forgetting require user confirmation.
- NEVER claim something is stored/forgotten without calling this tool and reporting its exact result (which contains the entry id). Paraphrasing an earlier successful store as "recorded" for new content is a fabrication — each new or changed fact needs a fresh tool call.
- Use list to inspect recent memories, including expired ones.
- Use forget to remove outdated or incorrect memory entries by id. Forgetting requires user confirmation.
- scope: "user" (default) for personal facts, "host" for machine-specific facts, "project" for codebase facts.
- ttl_seconds: optional expiry. Expired entries are not injected into context. Use /memory to review expired entries.
- TTL volatility judgment (you decide, policy anchors). Judge by the USER'S STATED INTENT AND lifetime cues, not by the fact's topic:
  - Volatile cues (short TTL <=604800): 临时/temporary, test/staging, 下线/回收 decommissioning soon, one-off ports, credentials that rotate.
  - Durable cues (omit ttl_seconds, or permanent if the user says forever): 之后都/以后都 from now on, 长期 long-term, 一直 always, production/designated hosts, stable architecture, deployment intent (e.g. 'this server is our cloud-database host from now on'). A server being an 'endpoint' does NOT make it volatile if the user framed it as a lasting arrangement.
  - If you omit ttl_seconds on an environment fact the policy still assigns a 7-day default; over-long proposals for volatile facts are capped. Host-scoped entries are always capped at 30 days. For durable facts outside the environment category, silence = no expiry.
- permanent: set true ONLY when the user EXPLICITLY framed the fact as lasting (e.g. "永久记住", "always remember this", "之后都/以后都用它", "from now on this is...", "don't let this expire"). It bypasses TTL defaults and caps (including the host-scope cap) and stores without expiry. The confirmation panel highlights it, and the user can reject. Never set permanent on your own initiative from a merely factual statement — the user's wording must carry the lasting intent. Do not combine permanent with ttl_seconds.
- reason: briefly explain why the memory is being stored or forgotten; for store, mention why the chosen TTL (or permanent) fits the fact's volatility and the user's words."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["search", "store", "forget", "list"],
                "description": "Memory operation to perform."
            },
            "query": {
                "type": "string",
                "description": "Search query for the search action."
            },
            "content": {
                "type": "string",
                "description": "Content to store for the store action."
            },
            "category": {
                "type": "string",
                "enum": ["preference", "environment", "solution", "pattern", "other"],
                "description": "Category for stored memory. Defaults to other."
            },
            "scope": {
                "type": "string",
                "enum": ["user", "host", "project"],
                "description": "Scope of the memory. Defaults to user. Use host for machine-specific facts, project for codebase facts."
            },
            "ttl_seconds": {
                "type": "integer",
                "description": "Optional time-to-live in seconds. When set, the memory expires after this duration and is excluded from context injection. Use 604800 for 7 days, 2592000 for 30 days. Do not set together with permanent."
            },
            "permanent": {
                "type": "boolean",
                "description": "Set true ONLY when the user explicitly asked for this fact to be remembered forever. Bypasses TTL defaults and caps (no expiry). The confirmation panel highlights it for user approval."
            },
            "reason": {
                "type": "string",
                "description": "Brief reason for storing or forgetting this memory. Shown in the confirmation prompt."
            },
            "memory_id": {
                "type": "integer",
                "description": "Memory id to forget for the forget action."
            }
        },
        "required": ["action"]
    })
}
