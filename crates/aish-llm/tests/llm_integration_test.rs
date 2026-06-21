// Integration tests for LLM Client, Provider Detection, and Sub-Session Isolation.
//
// This test module verifies:
// 1. LlmClient construction and provider prefix stripping
// 2. Message types handle all roles (system, user, assistant, tool)
// 3. Provider detection from model names and API bases
// 4. DiagnoseAgent construction and system prompt
// 5. SubSessionConfig defaults

use aish_llm::{
    detect_provider, detect_provider_from_model, refine_provider_from_api_base, ChatMessage,
    DiagnoseAgent, LlmClient, SubSessionConfig,
};

#[test]
fn test_llm_client_construction() {
    let client = LlmClient::new("https://api.openai.com/v1", "sk-test-key", "gpt-4o");
    assert_eq!(client.model_name(), "gpt-4o");
    assert_eq!(client.api_base(), "https://api.openai.com/v1");
    assert_eq!(client.api_key(), "sk-test-key");
}

#[test]
fn test_provider_prefix_stripping() {
    let client1 = LlmClient::new("https://api.openai.com/v1", "sk-test", "openai/gpt-4o");
    assert_eq!(client1.model_name(), "gpt-4o");

    let client2 = LlmClient::new(
        "https://api.openai.com/v1",
        "sk-test",
        "anthropic/claude-3-opus-20240229",
    );
    assert_eq!(client2.model_name(), "claude-3-opus-20240229");

    let client3 = LlmClient::new("https://api.openai.com/v1", "sk-test", "google/gemini-pro");
    assert_eq!(client3.model_name(), "gemini-pro");

    let client4 = LlmClient::new(
        "https://api.openai.com/v1",
        "sk-test",
        "deepseek/deepseek-coder",
    );
    assert_eq!(client4.model_name(), "deepseek-coder");
}

#[test]
fn test_message_conversion_all_roles() {
    let system_msg = ChatMessage::system("You are a helpful assistant");
    assert_eq!(system_msg.role, "system");
    assert_eq!(
        system_msg.text_content(),
        Some("You are a helpful assistant")
    );

    let user_msg = ChatMessage::user("Hello, how are you?");
    assert_eq!(user_msg.role, "user");
    assert_eq!(user_msg.text_content(), Some("Hello, how are you?"));

    let asst_msg = ChatMessage::assistant("I'm doing well, thank you!");
    assert_eq!(asst_msg.role, "assistant");
    assert_eq!(asst_msg.text_content(), Some("I'm doing well, thank you!"));
}

#[test]
fn test_provider_detection_from_model() {
    let openai_provider = detect_provider_from_model("gpt-4o");
    assert_eq!(openai_provider.id, "openai");
    assert_eq!(openai_provider.display_name, "OpenAI");
    assert!(openai_provider.supports_streaming);
    assert!(openai_provider.supports_tools);

    let anthropic_provider = detect_provider_from_model("claude-3-opus-20240229");
    assert_eq!(anthropic_provider.id, "anthropic");
    assert_eq!(anthropic_provider.display_name, "Anthropic");

    let google_provider = detect_provider_from_model("gemini-pro");
    assert_eq!(google_provider.id, "google");
    assert_eq!(google_provider.display_name, "Google AI");

    let deepseek_provider = detect_provider_from_model("deepseek-coder");
    assert_eq!(deepseek_provider.id, "deepseek");
    assert_eq!(deepseek_provider.display_name, "DeepSeek");

    let mistral_provider = detect_provider_from_model("llama3");
    assert_eq!(mistral_provider.id, "mistral");
    assert_eq!(mistral_provider.display_name, "Mistral AI");

    let unknown_provider = detect_provider_from_model("unknown-model");
    assert_eq!(unknown_provider.id, "unknown");
    assert_eq!(unknown_provider.display_name, "Unknown");
}

#[test]
fn test_provider_refinement_from_api_base() {
    let mut provider = detect_provider_from_model("unknown-model");
    assert_eq!(provider.id, "unknown");

    refine_provider_from_api_base(&mut provider, "https://api.openai.com/v1");
    assert_eq!(provider.id, "openai");
    assert_eq!(provider.display_name, "OpenAI");

    let mut provider2 = detect_provider_from_model("unknown-model");
    refine_provider_from_api_base(&mut provider2, "https://api.anthropic.com/v1");
    assert_eq!(provider2.id, "anthropic");

    let mut provider3 = detect_provider_from_model("unknown-model");
    refine_provider_from_api_base(&mut provider3, "http://localhost:11434/v1");
    assert_eq!(provider3.id, "ollama");
    assert_eq!(provider3.display_name, "Ollama (Local)");
    assert!(provider3.supports_tools);
}

#[test]
fn test_combined_provider_detection() {
    let provider1 = detect_provider("gpt-4", "https://api.openai.com/v1");
    assert_eq!(provider1.id, "openai");

    let provider2 = detect_provider("unknown-model", "http://localhost:11434/v1");
    assert_eq!(provider2.id, "ollama");

    let provider3 = detect_provider("claude-3-opus", "");
    assert_eq!(provider3.id, "anthropic");
}

#[test]
fn test_diagnose_agent_construction() {
    let _agent = DiagnoseAgent::new();

    let config = SubSessionConfig {
        max_iterations: 20,
        max_context_messages: 100,
        system_prompt: Some("Custom prompt".to_string()),
    };
    let _agent2 = DiagnoseAgent::with_config(config);
}

#[test]
fn test_diagnose_system_prompt() {
    use aish_llm::diagnose_agent::build_diagnose_prompt;
    let prompt = build_diagnose_prompt();

    assert!(prompt.contains("system diagnosis expert"));
    assert!(prompt.contains("System Information:"));
    assert!(prompt.contains("Hostname:"));
    assert!(prompt.contains("User:"));
    assert!(prompt.contains("OS:"));
    assert!(prompt.contains("Kernel:"));
    assert!(prompt.contains("Thought:"));
    assert!(prompt.contains("Action:"));
    assert!(prompt.contains("Observation:"));
    assert!(prompt.contains("Final Answer:"));
}

#[test]
fn test_subsession_config_default() {
    let config = SubSessionConfig::default();
    assert_eq!(config.max_context_messages, 50);
    assert_eq!(config.max_iterations, 10);
    assert!(config.system_prompt.is_none());
}

#[test]
fn test_subsession_custom_config() {
    let config = SubSessionConfig {
        max_context_messages: 100,
        max_iterations: 20,
        system_prompt: Some("Custom system prompt".to_string()),
    };
    assert_eq!(config.max_context_messages, 100);
    assert_eq!(config.max_iterations, 20);
    assert_eq!(
        config.system_prompt,
        Some("Custom system prompt".to_string())
    );
}

#[test]
fn test_llm_client_api_base_trimming() {
    let client = LlmClient::new("https://api.openai.com/v1/", "sk-test", "gpt-4o");
    assert_eq!(client.api_base(), "https://api.openai.com/v1");
}

#[test]
fn test_message_with_none_content() {
    use aish_llm::MessageContent;
    let msg_with_content = ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text("Hello".to_string())),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
    };
    assert_eq!(msg_with_content.text_content(), Some("Hello"));

    let msg_without_content = ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(vec![]),
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
    };
    assert_eq!(msg_without_content.text_content(), None);
}

#[test]
fn test_provider_dashboard_urls() {
    let openai = detect_provider_from_model("gpt-4");
    assert_eq!(
        openai.dashboard_url.as_deref(),
        Some("https://platform.openai.com/usage")
    );

    let anthropic = detect_provider_from_model("claude-3");
    assert_eq!(
        anthropic.dashboard_url.as_deref(),
        Some("https://console.anthropic.com/")
    );

    let google = detect_provider_from_model("gemini-pro");
    assert_eq!(
        google.dashboard_url.as_deref(),
        Some("https://aistudio.google.com/")
    );

    let unknown = detect_provider_from_model("unknown-model");
    assert!(unknown.dashboard_url.is_none());
}

#[test]
fn test_provider_tool_support_detection() {
    let openai = detect_provider_from_model("gpt-4");
    assert!(openai.supports_tools);

    let anthropic = detect_provider_from_model("claude-3");
    assert!(anthropic.supports_tools);

    let google = detect_provider_from_model("gemini-pro");
    assert!(google.supports_tools);

    let ollama = {
        let mut p = detect_provider_from_model("llama3");
        refine_provider_from_api_base(&mut p, "http://localhost:11434/v1");
        p
    };
    assert!(ollama.supports_tools);

    let unknown = detect_provider_from_model("unknown-model");
    assert!(!unknown.supports_tools);
}
