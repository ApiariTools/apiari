//! Email/IMAP skill — teaches the coordinator about email inbox monitoring.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_email {
        return None;
    }

    let names = ctx.email_names.join(", ");

    Some(format!(
        "## Email (IMAP)\n\
         This workspace has {count} email watcher(s) configured: {names}.\n\
         Email signals appear in the signal store with source `{{name}}_email_review_queue`.\n\n\
         Configuration is in {config} under `[[watchers.email]]`:\n\
         ```toml\n\
         [[watchers.email]]\n\
         name = \"fastmail\"\n\
         host = \"imap.fastmail.com\"\n\
         port = 993\n\
         tls = true\n\
         username = \"user@example.com\"\n\
         password = \"app-password\"\n\
         folder = \"INBOX\"\n\
         filter = \"UNSEEN\"\n\
         include_body = false\n\
         \n\
         [watchers.email.summarizer]   # optional Ollama summarization\n\
         base_url = \"http://localhost:11434\"\n\
         model = \"llama3.2:3b\"\n\
         ```\n\n\
         Email watchers connect via IMAP, poll for messages matching the filter,\n\
         and create signals with sender, subject, and optional body summary.\n\
         The optional `summarizer` section enables local LLM summarization of email bodies.\n",
        count = ctx.email_names.len(),
        config = ctx.config_path.display(),
    ))
}
