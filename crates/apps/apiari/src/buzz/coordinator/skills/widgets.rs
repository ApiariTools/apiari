//! Dashboard widget skill — teaches auto bots how to write data to the dashboard.

use super::SkillContext;

pub fn build_prompt(_ctx: &SkillContext) -> String {
    "## Dashboard Widgets\n\
     You can write structured data cards to the workspace dashboard using the `[WIDGET]` action marker.\n\
     Use this when you have data worth displaying persistently — CI status, metrics, task counts, \
     recent activity, alerts. Do NOT use it for conversational responses.\n\n\
     ### Syntax\n\
     ```\n\
     [WIDGET: slot-name]\n\
     {widget JSON}\n\
     [/WIDGET]\n\
     ```\n\
     With optional TTL in minutes (widget expires after this duration):\n\
     ```\n\
     [WIDGET: slot-name | 60]\n\
     {widget JSON}\n\
     [/WIDGET]\n\
     ```\n\n\
     - **slot-name**: unique string id for this widget (e.g. `ci-status`, `pr-queue`, `deploy-health`). \
       Same slot overwrites the previous value.\n\
     - **TTL**: omit for permanent; set to `1440` for 24h, `60` for 1h, etc.\n\n\
     ### Widget Types\n\
     Every widget needs `type`, `slot` (same as the slot-name above), and `title`.\n\n\
     **stat** — single metric:\n\
     ```json\n\
     {\"type\": \"stat\", \"slot\": \"ci-status\", \"title\": \"CI\", \"value\": \"3 failing\", \"status\": \"error\"}\n\
     ```\n\n\
     **stat_row** — row of metrics:\n\
     ```json\n\
     {\"type\": \"stat_row\", \"slot\": \"pipeline\", \"title\": \"Pipeline\",\n\
      \"stats\": [{\"label\": \"Passing\", \"value\": \"12\", \"status\": \"ok\"}, {\"label\": \"Failing\", \"value\": \"3\", \"status\": \"error\"}]}\n\
     ```\n\n\
     **list** — item list:\n\
     ```json\n\
     {\"type\": \"list\", \"slot\": \"failing-tests\", \"title\": \"Failing Tests\",\n\
      \"items\": [{\"id\": \"1\", \"label\": \"E2E login test\", \"status\": \"error\", \"href\": \"https://...\"}]}\n\
     ```\n\n\
     **alert_banner** — prominent callout:\n\
     ```json\n\
     {\"type\": \"alert_banner\", \"slot\": \"deploy-block\", \"title\": \"Deploy Blocked\",\n\
      \"body\": \"Main branch CI failing for 6 consecutive runs.\", \"severity\": \"error\"}\n\
     ```\n\n\
     **activity_feed** — timeline of events:\n\
     ```json\n\
     {\"type\": \"activity_feed\", \"slot\": \"recent-activity\", \"title\": \"Recent Activity\",\n\
      \"items\": [{\"id\": \"1\", \"timestamp\": \"2024-01-01T00:00:00Z\", \"event\": \"CI failed on main\", \"kind\": \"error\"}]}\n\
     ```\n\n\
     **progress** — progress bar:\n\
     ```json\n\
     {\"type\": \"progress\", \"slot\": \"migration\", \"title\": \"Migration\", \"label\": \"Step 3 of 5\", \"percent\": 60, \"status\": \"running\"}\n\
     ```\n\n\
     **markdown_block** — freeform text:\n\
     ```json\n\
     {\"type\": \"markdown_block\", \"slot\": \"notes\", \"title\": \"Notes\", \"content\": \"## Summary\\n...\"}\n\
     ```\n\n\
     Status values: `ok` `warning` `error` `neutral` `running` `pending`\n\n\
     ### Example — CI failure summary\n\
     ```\n\
     [WIDGET: ci-failures | 120]\n\
     {\"type\": \"list\", \"slot\": \"ci-failures\", \"title\": \"CI Failures\",\n\
      \"items\": [\n\
        {\"id\": \"1\", \"label\": \"E2E login\", \"meta\": \"main · 6 runs\", \"status\": \"error\"},\n\
        {\"id\": \"2\", \"label\": \"QuickDispatch error msg\", \"meta\": \"main · 3 runs\", \"status\": \"error\"}\n\
      ]}\n\
     [/WIDGET]\n\
     ```\n"
        .to_string()
}
