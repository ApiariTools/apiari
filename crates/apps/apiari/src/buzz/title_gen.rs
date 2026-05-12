//! On-device title generation using the `apfel` CLI (Apple Foundation Models).
//!
//! Single-shot, no history — just goal + recent output → `{"title": "...", "confidence": 85}`.

/// Call `apfel` to generate a short display title for a worker.
///
/// Returns `(title, confidence 0–100)` on success, `None` if apfel is
/// unavailable, times out, or returns unparseable output.
///
/// `recent_output` should be the last few hundred chars of agent output;
/// when present it lets the model reflect actual progress rather than just
/// the original goal, which is why confidence rises over time.
pub async fn generate_worker_title(
    goal: &str,
    recent_output: Option<&str>,
) -> Option<(String, u8)> {
    let context = match recent_output {
        Some(s) if !s.trim().is_empty() => {
            // Cap at 800 chars to keep the prompt short
            let snippet = if s.len() > 800 { &s[s.len() - 800..] } else { s };
            format!("\n\nRecent progress:\n{snippet}")
        }
        _ => String::new(),
    };

    let prompt = format!(
        "Generate a short display title (4-8 words) for this worker task.\n\
         Output JSON only, no explanation: {{\"title\": \"...\", \"confidence\": 85}}\n\
         Confidence is 0–100: low (~30) when only the goal is known, \
         high (~90) when recent progress confirms what the task actually does.\n\n\
         Goal: {goal}{context}"
    );

    let child = tokio::process::Command::new("apfel")
        .args(["-s", "You generate concise task titles. Respond with JSON only."])
        .arg(&prompt)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        child.wait_with_output(),
    )
    .await
    .ok()?
    .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let title = val["title"].as_str()?.trim().to_string();
    let confidence = val["confidence"].as_u64()?.min(100) as u8;

    if title.is_empty() {
        return None;
    }

    Some((title.chars().take(80).collect(), confidence))
}
