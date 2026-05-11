//! Dashboard widget skill — teaches auto bots how to write data to the dashboard.

use super::SkillContext;

pub fn build_prompt(_ctx: &SkillContext) -> String {
    "## Dashboard Widgets\n\
     When you have data worth displaying persistently on the dashboard — CI status, metrics, \
     task counts, recent activity, alerts — return a structured JSON response instead of plain text:\n\n\
     ```json\n\
     {\"text\": \"Your conversational summary here.\", \"widgets\": [{...}, {...}]}\n\
     ```\n\n\
     - **text** — required. A brief human-readable summary of what you found.\n\
     - **widgets** — array of widget objects. Each is written to the dashboard by slot. \
       Same slot overwrites the previous value.\n\n\
     Every widget requires `type`, `slot` (unique string id), and `title`.\n\n\
     ### Widget types\n\n\
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
     **activity_feed** — timeline:\n\
     ```json\n\
     {\"type\": \"activity_feed\", \"slot\": \"recent\", \"title\": \"Recent Activity\",\n\
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
     ```json\n\
     {\n\
       \"text\": \"CI has been failing on main for 6 consecutive runs across 2 test suites.\",\n\
       \"widgets\": [\n\
         {\n\
           \"type\": \"list\",\n\
           \"slot\": \"ci-failures\",\n\
           \"title\": \"CI Failures\",\n\
           \"items\": [\n\
             {\"id\": \"1\", \"label\": \"E2E login\", \"meta\": \"main · 6 runs\", \"status\": \"error\"},\n\
             {\"id\": \"2\", \"label\": \"QuickDispatch error msg\", \"meta\": \"main · 3 runs\", \"status\": \"error\"}\n\
           ]\n\
         }\n\
       ]\n\
     }\n\
     ```\n"
        .to_string()
}
