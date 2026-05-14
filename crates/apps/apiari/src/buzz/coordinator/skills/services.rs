//! Generic external-services skill — tells the coordinator where to find credentials.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if ctx.services.is_empty() {
        return None;
    }

    let mut prompt = format!(
        "## External Services\n\
         The following external services have credentials stored in `[services.*]` sections of {}.\n\
         Read the relevant section before making API calls — never hardcode credentials.\n\n",
        ctx.config_path.display(),
    );

    for (name, fields) in &ctx.services {
        if fields.is_empty() {
            prompt.push_str(&format!("- **{name}** (no fields configured)\n"));
        } else {
            prompt.push_str(&format!("- **{name}** — fields: {}\n", fields.join(", ")));
        }
    }

    Some(prompt)
}
