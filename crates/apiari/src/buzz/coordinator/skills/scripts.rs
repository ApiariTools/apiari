//! Scripts skill — teaches the coordinator about user-defined script watchers.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_scripts {
        return None;
    }

    let scripts_dir = dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".config/apiari/scripts");

    let mut prompt = format!(
        "## Scripts\n\
         User-defined script watchers are configured. Scripts directory: `{}`\n\
         Configured scripts: {}\n\n\
         Signal sources from scripts use the format `script_{{name}}` (e.g. `script_fly-deploys`).\n\n\
         You CAN write new scripts to the scripts directory when the user asks. Script conventions:\n\
         - First lines should be comments documenting the script\n\
         - Use `# Requires:` to list required CLI tools or env vars\n\
         - Use `# Params:` to list environment variables the script uses\n\
         - Scripts must be executable (`chmod +x`)\n\
         - Scripts should exit 0 on success, non-zero on failure\n\
         - stdout and stderr are each captured up to 10KB independently\n\
         - When `emit_on_change = true`, signals are only emitted when output changes\n",
        scripts_dir.display(),
        ctx.script_names.join(", "),
    );

    prompt.push_str(
        "\nTo read a script's description, check the first few comment lines of the script file.\n",
    );

    Some(prompt)
}
