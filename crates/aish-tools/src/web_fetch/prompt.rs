pub(crate) const DESCRIPTION: &str =
    "Fetch public web content and answer a focused prompt about the page.";

pub(crate) const PROMPT: &str = r#"Use this tool to fetch public web content and answer a focused question about it.

Usage:
- Use this tool when you need to retrieve and analyze public web content.
- If an authenticated MCP web fetch tool is available, prefer that tool for private or authenticated services.
- WebFetch will fail for authenticated or private URLs such as Google Docs, Confluence, Jira, private GitHub pages, localhost, and internal services.
- Provide a fully-qualified URL and a focused prompt describing what to extract from the page.
- HTTP URLs are automatically upgraded to HTTPS.
- HTML content is converted to readable text before being processed by a secondary model.
- Results may be summarized or truncated when the page is very large.
- A 15-minute cache is used for repeated requests to the same URL.
- If a URL redirects to a different host, call WebFetch again with the redirect URL returned by the tool.
- For GitHub URLs, prefer gh via bash when repository metadata, issues, PRs, releases, or API data are needed."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "Fully-qualified URL to fetch content from."
            },
            "prompt": {
                "type": "string",
                "description": "Focused prompt describing what information to extract from the fetched page."
            }
        },
        "required": ["url", "prompt"],
        "additionalProperties": false
    })
}
