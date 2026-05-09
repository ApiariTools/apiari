# @apiari/ui

Shared UI primitives and design tokens for the apiari platform. React components + CSS theme.

## Usage

```ts
import { PageHeader, StatusBadge, ObjectRow, EmptyState } from "@apiari/ui";
import "@apiari/ui/theme.css"; // load CSS variables (do this once, in your app entry)
```

## Primitives

| Component | Description |
|-----------|-------------|
| `DocumentSurface` | Scrollable content area with standard padding |
| `EmptyState` | Centered empty-state message with optional icon |
| `InspectorPane` | Side panel for detail/property views |
| `ModeScaffold` | Full-page layout with header + content slots |
| `ObjectRow` | Single-row list item with label, meta, and action slots |
| `PageHeader` | Section header with title, subtitle, and action button |
| `StatusBadge` | Colored pill badge (maps status strings to colors) |
| `ToolPanel` | Collapsible panel with a title bar and body slot |

## Theme

`src/theme.css` defines all CSS variables used across the platform:

```css
--bg: #111          --bg-card: #191919     --border: #282828
--text: #aaa        --text-strong: #eee    --text-faint: #555
--accent: #f5c542   --red: #e85555         --green: #5cb85c
```

Import this stylesheet once in your app entry. Do **not** import it from individual components.

## Rules

- React and ReactDOM are **peer dependencies** — do not bundle them.
- Use CSS Modules (`.module.css`) co-located with each component, not global CSS.
- Icons: `lucide-react` only — no emoji icons.
- No API or data-fetching logic — purely presentational.
- No `@apiari/types` or `@apiari/api` imports — keep this package dependency-free.
