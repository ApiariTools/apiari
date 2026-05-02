//! Scripts skill — teaches the coordinator about user-defined script watchers.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> String {
    let scripts_dir = crate::config::config_dir().join("scripts");

    if !ctx.has_scripts {
        return format!(
            "## Scripts\n\
             No script watchers are configured yet. You can set up user-defined script watchers \
             by adding `[[watchers.script]]` entries to the workspace config and creating scripts \
             in `{}`.\n\
             Scripts should document `# Requires:` and `# Params:` at the top, \
             exit 0 on success / non-zero on failure, and be `chmod +x`.\n\
             Ask the user if they'd like to set one up.\n",
            scripts_dir.display(),
        );
    }

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
         - stdout and stderr are each capped at 10KB independently (up to 20KB combined)\n\
         - When `emit_on_change = true`, signals are only emitted when output changes\n",
        scripts_dir.display(),
        ctx.script_names.join(", "),
    );

    prompt.push_str(
        "\nTo read a script's description, check the first few comment lines of the script file.\n",
    );

    prompt
}
