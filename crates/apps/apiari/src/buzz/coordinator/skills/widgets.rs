//! Dashboard widget skill — teaches auto bots how to write data to the dashboard.

use super::SkillContext;

pub fn build_prompt(_ctx: &SkillContext) -> String {
    "## Dashboard Widgets\n\
     When you have data worth displaying — CI status, errors, assigned issues, review queues, \
     activity — return a structured JSON response instead of plain text:\n\n\
     ```json\n\
     {\"text\": \"Your conversational summary here.\", \"widgets\": [{...}, {...}]}\n\
     ```\n\n\
     - **text** — required. A brief human-readable summary.\n\
     - **widgets** — array of widget objects, each written to the dashboard by `slot`. Same slot overwrites the previous value.\n\n\
     Every widget requires `type`, `slot` (unique string id), and `title`.\n\
     Optional base fields on any widget: `href` (makes the title a link), `source` (e.g. `\"GitHub\"`), `updated_at`.\n\n\
     ### Widget Quality Rules\n\n\
     1. **Show the specific items, not just a count or percentage.** If 3 CI runs are failing, \
     list which 3. If 5 Linear issues are assigned, list which 5. A `stat` saying \"3 failing\" \
     is useless without a `list` showing what's failing.\n\
     2. **Make everything clickable.** Every list item should have `href` if a URL is available. \
     Set `href` on the widget title to link to the relevant dashboard (GitHub Actions, Linear board, Sentry, etc.).\n\
     3. **Add an `alert_banner` when something is broken or needs urgent attention.** \
     Broken CI, a spiking error, a blocked deploy — lead with an `alert_banner`, then follow with the detail `list`.\n\
     4. **Add context via `meta`.** Each list item should include a `meta` string with the key detail: \
     branch name, run number, priority, assignee, event count, last seen — whatever helps at a glance.\n\
     5. **Combine widget types.** A good report is usually 2–3 widgets: an `alert_banner` (if broken) + \
     a `list` of specifics + optionally a `stat_row` summary. Never just one vague aggregate.\n\n\
     ### Widget types\n\n\
     **stat** — single metric (use for healthy/neutral aggregates, not for failures):\n\
     ```json\n\
     {\"type\": \"stat\", \"slot\": \"open-prs\", \"title\": \"Open PRs\", \"value\": \"4\", \"status\": \"neutral\", \"href\": \"https://github.com/org/repo/pulls\"}\n\
     ```\n\n\
     **stat_row** — row of metrics:\n\
     ```json\n\
     {\"type\": \"stat_row\", \"slot\": \"pipeline\", \"title\": \"Pipeline\",\n\
      \"stats\": [{\"label\": \"Passing\", \"value\": \"12\", \"status\": \"ok\"}, {\"label\": \"Failing\", \"value\": \"3\", \"status\": \"error\"}]}\n\
     ```\n\n\
     **list** — item list with links (preferred widget for failures, reviews, and assignments):\n\
     ```json\n\
     {\"type\": \"list\", \"slot\": \"failing-ci\", \"title\": \"Failing Jobs\", \"href\": \"https://github.com/org/repo/actions\",\n\
      \"items\": [{\"id\": \"1\", \"label\": \"E2E login test\", \"meta\": \"main · run #42\", \"status\": \"error\", \"href\": \"https://github.com/org/repo/actions/runs/42\"}]}\n\
     ```\n\n\
     **alert_banner** — prominent callout for critical state:\n\
     ```json\n\
     {\"type\": \"alert_banner\", \"slot\": \"ci-alert\", \"title\": \"CI Failing on main\",\n\
      \"body\": \"3 consecutive failures. Deploy is blocked.\", \"severity\": \"error\", \"href\": \"https://github.com/org/repo/actions\"}\n\
     ```\n\n\
     **status_grid** — grid of status pills (good for multi-repo or multi-check overviews):\n\
     ```json\n\
     {\"type\": \"status_grid\", \"slot\": \"repo-health\", \"title\": \"Repo Health\",\n\
      \"items\": [{\"id\": \"api\", \"label\": \"api\", \"status\": \"ok\", \"href\": \"https://...\"}, {\"id\": \"web\", \"label\": \"web\", \"status\": \"error\", \"href\": \"https://...\"}]}\n\
     ```\n\n\
     **activity_feed** — timeline of recent events:\n\
     ```json\n\
     {\"type\": \"activity_feed\", \"slot\": \"recent\", \"title\": \"Recent Activity\",\n\
      \"items\": [{\"id\": \"1\", \"timestamp\": \"2024-01-01T00:00:00Z\", \"event\": \"CI failed on main\", \"kind\": \"error\", \"href\": \"https://...\"}]}\n\
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
     ### Examples by data source\n\n\
     **GitHub CI failures** — run `gh run list --repo {repo} --status failure --json name,url,headBranch,createdAt --limit 10` to get specifics:\n\
     ```json\n\
     {\"text\": \"CI failing on main for 3 consecutive runs.\",\n\
      \"widgets\": [\n\
       {\"type\": \"alert_banner\", \"slot\": \"ci-alert\", \"title\": \"CI Failing on main\",\n\
        \"body\": \"3 consecutive failures across 2 jobs.\", \"severity\": \"error\", \"href\": \"https://github.com/org/repo/actions\"},\n\
       {\"type\": \"list\", \"slot\": \"ci-failures\", \"title\": \"Failing Jobs\", \"href\": \"https://github.com/org/repo/actions\",\n\
        \"items\": [\n\
         {\"id\": \"1\", \"label\": \"E2E login test\", \"meta\": \"main · 3 runs\", \"status\": \"error\", \"href\": \"https://github.com/org/repo/actions/runs/123\"},\n\
         {\"id\": \"2\", \"label\": \"Rust clippy\", \"meta\": \"main · 1 run\", \"status\": \"error\", \"href\": \"https://github.com/org/repo/actions/runs/124\"}\n\
        ]}\n\
     ]}\n\
     ```\n\n\
     **Linear assigned/mentioned issues** — list by priority with direct links:\n\
     ```json\n\
     {\"text\": \"4 Linear issues need attention, 1 is urgent.\",\n\
      \"widgets\": [\n\
       {\"type\": \"list\", \"slot\": \"linear-assigned\", \"title\": \"Linear Issues\", \"href\": \"https://linear.app\",\n\
        \"items\": [\n\
         {\"id\": \"ENG-42\", \"label\": \"Fix auth token expiry\", \"meta\": \"Urgent · In Progress\", \"status\": \"error\", \"href\": \"https://linear.app/team/issue/ENG-42\"},\n\
         {\"id\": \"ENG-38\", \"label\": \"Add pagination to API\", \"meta\": \"Medium · Todo\", \"status\": \"neutral\", \"href\": \"https://linear.app/team/issue/ENG-38\"}\n\
        ]}\n\
     ]}\n\
     ```\n\n\
     **Sentry errors** — list unresolved issues with event counts and last-seen:\n\
     ```json\n\
     {\"text\": \"2 unresolved Sentry errors, one is spiking.\",\n\
      \"widgets\": [\n\
       {\"type\": \"list\", \"slot\": \"sentry-errors\", \"title\": \"Sentry Errors\", \"href\": \"https://sentry.io/organizations/org/issues/\",\n\
        \"items\": [\n\
         {\"id\": \"1\", \"label\": \"TypeError: Cannot read 'id'\", \"meta\": \"142 events · 2h ago\", \"status\": \"error\", \"href\": \"https://sentry.io/organizations/org/issues/1/\"},\n\
         {\"id\": \"2\", \"label\": \"KeyError: 'user_id' in session\", \"meta\": \"8 events · 1d ago\", \"status\": \"warning\", \"href\": \"https://sentry.io/organizations/org/issues/2/\"}\n\
        ]}\n\
     ]}\n\
     ```\n\n\
     **GitHub PRs needing review** — list with author and label context:\n\
     ```json\n\
     {\"text\": \"3 PRs are waiting for your review.\",\n\
      \"widgets\": [\n\
       {\"type\": \"list\", \"slot\": \"prs-to-review\", \"title\": \"PRs to Review\", \"href\": \"https://github.com/pulls\",\n\
        \"items\": [\n\
         {\"id\": \"101\", \"label\": \"Add rate limiting to API\", \"meta\": \"@alice · 2d ago\", \"status\": \"warning\", \"href\": \"https://github.com/org/repo/pull/101\"},\n\
         {\"id\": \"98\", \"label\": \"Fix mobile layout\", \"meta\": \"@bob · 4h ago\", \"status\": \"neutral\", \"href\": \"https://github.com/org/repo/pull/98\"}\n\
        ]}\n\
     ]}\n\
     ```\n"
        .to_string()
}
