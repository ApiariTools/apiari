//! Apiari system skill — teaches the coordinator about apiari's own config
//! system: skills, authority levels, capabilities, and signal hook wiring.

use super::SkillContext;
use crate::config::WorkspaceAuthority;

/// Build the apiari system prompt. Always included.
pub fn build_prompt(ctx: &SkillContext) -> String {
    let root = ctx.workspace_root.display();
    let authority_str = match ctx.authority {
        WorkspaceAuthority::Observe => "observe",
        WorkspaceAuthority::Autonomous => "autonomous",
    };

    format!(
        "## Apiari System\n\
         You are running inside apiari. This section explains how the apiari config \
         system works so you can help users set things up.\n\n\
         ### Authority & Capabilities\n\
         The workspace authority level is `{authority_str}`. Authority is set in the workspace config:\n\
         ```toml\n\
         authority = \"autonomous\"   # full toolset (default)\n\
         # authority = \"observe\"    # read-only tools only, no Bash or swarm dispatch\n\
         ```\n\n\
         Fine-grained capabilities:\n\
         ```toml\n\
         [capabilities]\n\
         dispatch_workers = true   # default true in autonomous, false in observe\n\
         merge_prs = false            # default — never allow merging\n\
         # merge_prs = \"on_command\"  # allow when user explicitly requests it\n\
         ```\n\n\
         ### Communication Style (Soul)\n\
         `{root}/.apiari/soul.md` is automatically loaded into every coordinator session. \
         Users should put communication style and behavioral guidelines here: tone, verbosity, \
         what to lead with, personality traits. If the file does not exist, no soul is loaded.\n\n\
         ### Project Context\n\
         `{root}/.apiari/context.md` is automatically loaded into every coordinator session. \
         Users should put high-level project info here: what the project is, the tech stack, \
         team ownership, key conventions, and anything the coordinator should always know. \
         If the file does not exist, no context is loaded.\n\n\
         ### Playbooks\n\
         Markdown files in `{root}/.apiari/skills/*.md` are indexed as playbooks. \
         The coordinator sees an index of names + first-line descriptions, but full content \
         is NOT loaded by default. Full playbook content is injected only when a signal hook \
         fires with a matching `skills` field.\n\n\
         To create a playbook, add a `.md` file under `.apiari/skills/`. The first line \
         (stripped of `#`) becomes the description in the index.\n\n\
         ### Signal Hook → Playbook Wiring\n\
         Connect a playbook to a signal hook with the `skills` field:\n\
         ```toml\n\
         [[coordinator.signal_hooks]]\n\
         source = \"github_ci_fail\"\n\
         action = \"Triage the CI failure\"   # one-sentence intent ONLY\n\
         skills = [\"ci-triage\"]              # loads .apiari/skills/ci-triage.md\n\
         ```\n\n\
         The `action` field should be a single sentence describing the intent — \
         the detailed process belongs in the playbook, not the hook. This keeps hooks \
         declarative and playbooks reusable.\n\n\
         ### Scaffolding\n\
         `apiari init` creates the workspace config and scaffolds:\n\
         - `.apiari/soul.md` — template for communication style\n\
         - `.apiari/context.md` — template for project context\n\
         - `.apiari/skills/` — empty directory for playbooks\n\n\
         Users can also create these manually.\n",
    )
}
