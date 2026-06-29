// Integration tests — require gateway running on :9000 and llama-server on :8081
// Run: cargo test --test integration -- --ignored (needs live services)
// Unit tests are inline in each module (router, context, etc.) — run via: cargo test

// Live E2E tests (require services up)
#[cfg(test)]
mod e2e {
    const GATEWAY: &str = "http://localhost:9000";

    #[tokio::test]
    #[ignore = "requires live gateway"]
    async fn health_returns_alive() {
        let resp = reqwest::get(format!("{GATEWAY}/health"))
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(resp["status"], "alive");
        assert_eq!(resp["layers"], 9);
    }

    #[tokio::test]
    #[ignore = "requires live gateway"]
    async fn models_returns_ornith() {
        let resp = reqwest::get(format!("{GATEWAY}/v1/models"))
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        let ids: Vec<&str> = resp["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert!(ids.contains(&"ornith"));
    }

    #[tokio::test]
    #[ignore = "requires live gateway"]
    async fn empty_messages_returns_400() {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{GATEWAY}/v1/chat/completions"))
            .json(&serde_json::json!({"model": "ornith", "messages": []}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    #[ignore = "requires live gateway"]
    async fn chat_returns_choices() {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{GATEWAY}/v1/chat/completions"))
            .json(&serde_json::json!({
                "model": "ornith",
                "messages": [{"role": "user", "content": "Say hello"}],
                "temperature": 0.0,
                "max_tokens": 64
            }))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert!(resp["choices"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false));
    }
}
