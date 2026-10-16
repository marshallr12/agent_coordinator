# Web UI refresh behavior

The dashboard (`web/app.js`) re-reads data on a timer, after navigation and
after a mutation. Every one of those paths updates the page through one shared
mechanism, so a refresh touches only the parts of the DOM whose data changed.
This chapter lists every view and refresh path, what each can change, and the
rules the mechanism follows. `scripts/test_ui_refresh.mjs` verifies them in
headless Chrome.

## The mechanism

- **Idempotent setters.** `setText`, `show`, `setClass`, `setAttr`, `setProp` and
  `setData` write only when the value differs. `setState` leaves an identical
  status panel alone. Setting a value that is already there still counts as a
  mutation to a `MutationObserver`, so the guard matters.
- **`reconcile(parent, descriptors)`** renders every keyed list. A descriptor is
  `{key, sig, build}`: `key` identifies the item (an id), `sig` is a string that
  changes exactly when what is displayed changes, and `build()` creates the node.
  A node whose key and signature match is kept. A node whose signature changed is
  rebuilt and replaced in place. New items are inserted, missing items removed,
  and nodes are moved only when their order changed.
- **`patchSection(container, signature, build)`** does the same for a region that
  is not a list (the lease card, the repository binding snippet).
- **State carried across a rebuild.** When a node is replaced, `captureUi` and
  `restoreUi` give the new node the focus, text selection, open `<details>` and
  scroll offsets of the old one, matched by position inside the node.
- **Signatures describe what is shown.** A list row's signature is its data. A
  section with buttons also includes the actor and any other value its buttons
  read. The lease card's signature is its rendered text, so a countdown that stays
  within the same displayed minute changes nothing.
- **Refresh gate.** `refreshBlocked(region)` stops a background refresh (poll,
  auto-refresh or an attachment reload) while the tab is hidden, a mutation or
  upload is pending, a dialog is open, or a text field, menu or file chooser
  inside the region being refreshed has focus. A response that arrives after the
  gate closed is discarded. Fields outside the region, such as the search box
  above the task list, never block it because the refresh does not touch them.
- **Visibility.** The 5-second timer is cleared when the tab becomes hidden. When
  it becomes visible the dashboard refreshes the current view once and restarts
  the timer.
- **Loading panels.** A "Loading…" panel appears only for a view that has nothing
  to show yet (first load, or a different project, task or filter). A reload of
  what is already displayed keeps it on screen and patches it.

The mechanism is vanilla JavaScript. No framework, no Alpine.js build and no
other dependency was added, and the Content-Security-Policy is unchanged.
Rendering still uses `textContent` and DOM construction only; no data reaches
`innerHTML`.

The freshness labels (`Updated h:mm` in the top bar, `Read h:mm` on Knowledge &
evidence) have minute resolution and change at most once a minute. The top bar
is outside every view.

## Refresh paths

| Path | Trigger | Views |
| --- | --- | --- |
| Poll | 5-second timer, gated by `refreshBlocked` | Overview, Task queue and Completed (only with **Auto-refresh** on), Task detail, Access (credentials), Resources |
| Load | Navigation (sidebar, brand button, tabs, "Open tasks", settings gear and its sub-pages), project or filter selects, **Refresh** buttons, **Load more** | Every view |
| Post-mutation | The success callback of `startMutation` | Overview (project created, policy saved), Task queue (task or objective created, task deleted or canceled), Completed (archive), Task detail (every task action, review, recovery, upload), Reports (resolve), Knowledge & evidence (record saved), Resources (created), Access (credential, operator, clock, restore), dialog lists (browser sessions) |
| Visibility | `visibilitychange` | The current view (one refresh on becoming visible) |
| Navigation | `showView` | Selects and tab state; the destination view's load path |

## Views

"Before" is what the view did on every refresh before this change.

| View | Loader and trigger | Before | What can change | Now |
| --- | --- | --- | --- | --- |
| Overview: summary cards | `renderSummary` after any project or task load | Cleared and rebuilt four cards | The four counts | Cards keyed by label; a card is rebuilt only when its number changes |
| Overview: project cards | `loadProjects`: load, **Refresh**, poll, post-mutation | Cleared and rebuilt the grid when the list changed; the loading panel flashed on every manual load | A project's name, repository, branch; projects added or removed | Keyed by project id, signature is the project record |
| Project selects (tasks, archive, knowledge) | `fillProjectSelect`, also on every entry to Tasks | Cleared and rebuilt all options | Project names and membership | Options keyed by value; the selected value is set only if it differs |
| Project settings | `openProjectSettings`, `loadIntegrationOwner` | Rewrote heading, binding snippet and owner line, blanking the owner line while it re-read | Project name, repository, branch, integration owner and policy revision | Text set only on change; binding snippet is a signed section; the owner line is not blanked for the project already shown |
| Task queue | `loadTasks`: load, **Refresh**, poll (Auto-refresh), post-mutation, search, status filter, page size | Cleared and rebuilt the page of rows when anything in the list changed | Any task field; tasks added, removed or moved between pages | Rows keyed by task id with the record as signature; page buttons keyed by index |
| Completed tasks | `loadCompletedTasks`: tab, **Refresh**, poll, archive | As the queue | Same, plus tasks leaving by archiving | Same rows, keyed by task id |
| Archived tasks | `loadArchivedTasks`: tab, project select, **Refresh**, back from detail | Hid the list and showed "Loading" on every load, then rebuilt it | Archived task fields; tasks restored | Cached rows stay while reloading; keyed by task id; skipped when the response is unchanged |
| Task detail: heading, status, brief | `loadTaskDetail`: open, poll, post-mutation | Rewrote every field, and hid the page with "Loading" after a mutation | Title, description, revision, status, kind, human-delivered flag | Each field set only on change; the page is blanked only when a different task opens |
| Task detail: acceptance criteria | same | Cleared and rebuilt | Edited criteria | Items keyed by position |
| Task detail: action buttons | `renderTaskActions` | Cleared and rebuilt every button, so a button could vanish under the pointer | Which actions apply: lifecycle, ownership, blocker, objective links, actor | Buttons keyed by action; signature is only the fields the buttons read, so a new checkpoint leaves them alone |
| Task detail: ownership and lease | `renderLease` | Cleared and rebuilt | Owner, state, lease minutes, expiry, generation | Signed section; signature is the rendered text |
| Task detail: lifecycle changes | `renderLifecycle` | Cleared and rebuilt | New lifecycle events or replacements | Keyed by event id and replaced task |
| Task detail: checkpoint trail | `renderCheckpoints` | Cleared and rebuilt every checkpoint | New checkpoints (existing ones are immutable) | Keyed by checkpoint id |
| Task detail: jobs and holds | `renderJobEvidence` | Cleared and rebuilt | Job state and observation, reservation state | Keyed by job and reservation id |
| Task detail: submission and workflow | `renderWorkflow` | Cleared and rebuilt, closing open details | Submission, activity state, blockers, integration-blocked report, next actions | Candidate, each activity, blocker, integration-blocked notice and next action keyed separately |
| Task detail: attachments | `loadTaskAttachments` once per task; quietly on each poll | Rebuilt on each load | Files added or finalized | Keyed by artifact id; a poll reloads quietly and keeps what is shown on error |
| Integrator reports | `loadReports`: open, filter, **Refresh**, **Load more**, resolve | Cleared the list and showed "Loading" on every load | A report's state or resolution; new reports | Cards keyed by report id; open "Report details" survives, even for a rebuilt card |
| Attention digest | `loadDigest`: open, **Refresh** | Cleared and rebuilt | The three section lists | Three section cards keyed by section |
| Autonomy | `loadAutonomy`: open, window select, **Refresh** | Cleared and rebuilt | Any figure of a card; a source that fails shows its error in its own card | Cards keyed by section; signature is the card's source data |
| Project card autonomy summary | `loadAutonomySummary` after each project load, at most once a minute per project | Not shown | Counts, last canary, today's cost | Text patched in place; the card is not rebuilt |
| Connected sessions | `loadAgentSessions`: open, window select, **Refresh** | Cleared and rebuilt | A session's activity and held attempts | Cards keyed by session id |
| Knowledge & evidence | `loadShared`: open, selects, **Refresh**, **Load more**, search, post-mutation | Cleared and rebuilt, discarding history a person had expanded | Record fields; the rules text of a context search | Cards keyed by kind and record id; expanded history survives while the record is unchanged |
| Resources | `loadResources`: open, **Refresh**, poll, **Load more**, create | Cleared and rebuilt every card | Held units, description; resources added | Cards keyed by resource id |
| Access: credentials | `loadCredentials`: open, **Refresh**, poll, post-mutation | Cleared and rebuilt every row | Revocation, rotation, new credentials | Rows keyed by credential id (signature also records whether the row offers a replacement) |
| Access: operators | `loadOperators`: open, **Refresh**, **Load more**, post-mutation | Cleared and rebuilt | Role, enabled, revision | Rows keyed by operator id |
| Access: server clock, restore status | `loadClock`, `loadRestore`: open, **Refresh**, post-mutation | Cleared and rebuilt | Clock incident; restore progress and holds | Status lines, evidence and holds keyed separately |
| Dialogs | Built once per open | n/a | Lists inside a dialog (browser sessions, objectives) reload after **Load more** or a revoke | Rows keyed by id, so revoking one session rebuilds only that row. Dialog forms are never re-rendered. Task history replaces its list when the record type changes, because that is a different dataset |

## Verification

`node --check web/app.js` checks syntax. `scripts/test_ui_refresh.mjs` runs
Chrome against a fixture server and, per view, requires:

1. an unchanged refresh by button, poll, navigation away and back, or finished
   mutation records no `MutationObserver` mutation inside the view;
2. changing one item rebuilds only that item's node, and inserting or removing
   one item leaves the others in place;
3. focus, selection, open `<details>`, scroll position and an open dialog
   survive;
4. polling is off while the tab is hidden, resumes on becoming visible, and does
   not refresh around an open dialog, a pending mutation or a focused field in
   the region.

Run it with `CHROME_BIN=/path/to/chrome node scripts/test_ui_refresh.mjs`. Set
`CHROME_ARGS=--no-sandbox` where Chrome's sandbox is unavailable.
