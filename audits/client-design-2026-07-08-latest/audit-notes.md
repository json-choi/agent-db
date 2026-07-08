# dopedb Client Design Audit

Date: 2026-07-08
Surface: latest running dev app, `/Users/jaesong/Documents/agent-db/target/debug/dopedb`, PID 91817
Destination: local folder

## Evidence

1. `01-current-dev-app.png` - Agent workspace empty state
2. `02-sql-tab.png` - SQL editor
3. `03-data-tab-no-table.png` - Data tab before table selection
4. `04-data-table-selected.png` - Data grid with `accounts` selected
5. `05-schema-tab.png` - Visual schema
6. `06-settings.png` - Safety settings

## Short Diagnosis

The app does not look bad because of one broken component. It feels weak because the layout has two opposite problems at the same time: dense database surfaces are overloaded, while important empty or setup states feel under-designed and adrift. The product promise is "safe database access for agents," but the UI still reads more like a generic dark DB browser with an Agent tab added on.

## What Is Working

- The left sidebar gives immediate orientation: connection, migrations, table search, table list, row counts.
- The Schema screen has the strongest concept. It visually communicates relationships and gives a detail panel.
- The read-only/write state is always visible in the header, which is the right product instinct.
- The Data grid supports real database work: filtering, pagination, export, edit, and structure access are all close to the data.

## Main UX Risks

1. The Agent workspace is too quiet for the product's main differentiator.
   Evidence: `01-current-dev-app.png`
   The empty state says what will happen, but it does not make the safe-agent workflow feel concrete. The counters on the right, segmented tabs, and MCP button are visually small. A first-time user may not understand that this is the central trust ledger for MCP activity.

2. The sidebar is doing too much visual work.
   Evidence: all screenshots
   Table names, icons, row counts, truncation, search, migration, connection status, refresh, settings, and selected table state all compete in a 240px column. Row counts are useful, but their current placement makes the table list noisy. Long names truncate in a way that makes scanning harder.

3. Empty states waste space instead of guiding action.
   Evidence: `03-data-tab-no-table.png`, `02-sql-tab.png`, `06-settings.png`
   Large dark areas make the app feel unfinished. The Data empty state has one centered sentence. The SQL screen has an editor plus a vast blank area. Settings puts critical safety controls in a small top-left column and leaves most of the work area unused.

4. The navigation model is visually split.
   Evidence: `01-current-dev-app.png`, `02-sql-tab.png`
   Data, Schema, SQL, History, and Audit sit together, while Agent is isolated on the far right. This may be intentional conceptually, but it reads like a bolt-on tab. Since Agent is dopedb's differentiator, it needs a clearer relationship to the rest of the workspace.

5. Data grid density is useful but uncurated.
   Evidence: `04-data-table-selected.png`
   Long IDs, OAuth scopes, tokens, and provider IDs dominate the first viewport. The user sees a lot of data but little meaning. The table needs better defaults: pinned important columns, hidden sensitive/noisy columns, smarter truncation, and a stronger details drawer.

6. Visual hierarchy is too flat.
   Evidence: all screenshots
   The dark theme uses many similar grays, thin borders, small type, and small controls. Headers, helper text, counters, fields, labels, and secondary actions often have similar visual weight. This makes the product feel less polished than the underlying functionality.

## Accessibility Risks From Screenshots

- Small labels and row-count text may be hard to read, especially in the sidebar and settings.
- Several muted gray text colors appear low contrast against the dark background.
- Target sizes in the table toolbar and sidebar rows look tight for repeated use.
- Focus and keyboard behavior were not verified. This audit is visual only.

## Opportunity Areas

1. Make Agent a trust ledger, not just an empty tab.
   Add a stronger empty state with three concrete promises: "What the agent can see," "What ran," and "What was blocked or approved." Show MCP connection status and setup as a primary panel.

2. Give each screen a density mode.
   Data and Schema can be dense. Agent, Settings, and empty states need medium-density panels with clearer grouping. Right now the app jumps between overloaded and underfilled.

3. Reduce sidebar noise.
   De-emphasize row counts, improve truncation, add table grouping/search feedback, and give selected table state more breathing room. Consider a collapsible/resizable sidebar for large schemas.

4. Curate the Data grid first viewport.
   Hide or collapse very long token/ID columns by default, pin the most human-readable columns, and move full cell inspection into a details panel. The current first viewport can expose sensitive-looking fields and makes the table feel harsher than necessary.

5. Rebuild Safety settings as a status card.
   Instead of a flat checklist, make the current policy obvious: "Read-only for agents," "Writes disabled/enabled," "Approval required," "Preview before write." Risky states should be visually distinct.

6. Tighten the design tokens.
   Increase type contrast, reduce competing border lines, define clearer page headings, and make primary/secondary/quiet controls more distinct.

## Recommended First Pass

1. Redesign Agent empty state and MCP status as the product's "safe agent access" home.
2. Clean up the sidebar hierarchy: table rows, row counts, selected states, and truncation.
3. Improve Data grid defaults for long/sensitive columns and add a stronger row/cell detail pattern.
4. Reframe Safety settings into readable policy cards instead of a small checklist.

## Evidence Limits

This audit used screenshots from the running dev app only. It did not verify hover states, keyboard navigation, screen reader output, resizing behavior, or live MCP activity after an agent call.
