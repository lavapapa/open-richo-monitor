use std::time::Duration;

fn contents(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 4000 {
        return Err("请填写 1–4000 字的意见。".into());
    }
    Ok(value)
}

#[tauri::command]
pub async fn submit_feedback(app: tauri::AppHandle, content: String) -> Result<(), String> {
    let content = contents(&content)?;
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../src/lib/support.json")).unwrap();
    let endpoint = config["feedbackUrl"]
        .as_str()
        .ok_or("反馈服务尚未开通，你可以先复制意见。")?;
    let response = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?
        .post(endpoint).header("Accept", "application/json")
        .json(&serde_json::json!({ "message": content, "version": app.package_info().version.to_string(), "platform": std::env::consts::OS }))
        .send().await.map_err(|_| "反馈未提交，请检查网络后重试。".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "反馈未提交（HTTP {}），请稍后重试。",
            response.status().as_u16()
        ));
    }
    let receipt: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "反馈服务返回了无法确认的结果，请稍后重试。".to_string())?;
    if receipt["ok"] != true {
        return Err("反馈服务未确认收到，请稍后重试。".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn feedback_requires_content_and_bounds_size() {
        assert_eq!(super::contents("  二维码太小  ").unwrap(), "二维码太小");
        assert!(super::contents(" \n ").is_err());
        assert!(super::contents(&"测".repeat(4001)).is_err());
    }
}
