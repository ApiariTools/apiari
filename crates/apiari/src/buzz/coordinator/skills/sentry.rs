//! Sentry API skill — teaches the coordinator how to query Sentry.

use super::SkillContext;

pub fn build_prompt(ctx: &SkillContext) -> Option<String> {
    if !ctx.has_sentry {
        return None;
    }

    Some(format!(
        "## Sentry\n\
         Read Sentry credentials from {} (`[watchers.sentry]` section: org, project, token).\n\
         Useful queries (via Bash with curl):\n\
         - List unresolved issues: `curl -sH \"Authorization: Bearer $TOKEN\" \
           \"https://sentry.io/api/0/projects/$ORG/$PROJECT/issues/?query=is:unresolved\"`\n\
         - Issue detail: `curl -sH \"Authorization: Bearer $TOKEN\" \
           \"https://sentry.io/api/0/issues/$ISSUE_ID/\"`\n\
         - Latest event: `curl -sH \"Authorization: Bearer $TOKEN\" \
           \"https://sentry.io/api/0/issues/$ISSUE_ID/events/latest/\"`\n\
         - Issue events: `curl -sH \"Authorization: Bearer $TOKEN\" \
           \"https://sentry.io/api/0/issues/$ISSUE_ID/events/\"`\n\
         Always pipe through `jq` for readability.\n",
        ctx.config_path.display(),
    ))
}
