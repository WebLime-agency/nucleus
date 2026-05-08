# Workspace Model

Nucleus uses a workspace, project, and session hierarchy.

## Workspace

The workspace is the root for one Nucleus install.

It owns:

- the workspace root path
- default main and utility model targets
- router profiles
- discovered projects

Workspace settings belong to Nucleus, not to a specific client.

The workspace also owns the default Utility Worker run budget: maximum steps, maximum actions, and wall-clock limit for a turn. Sessions inherit that default unless the user selects a session-level run budget preset.

## Project

Projects are directories discovered under the workspace root.

They are local working contexts, not separate installs. A project can contribute prompt context, routing defaults, and file scope, but it does not replace the workspace as the top-level authority.

Projects should be activatable per session. A user must be able to work with:

- no active project for ad hoc tasks
- one active project for focused work
- multiple active projects when cross-repo context is necessary

## Session

Sessions are the unit of AI work.

A session may be:

- ad hoc with no attached project
- anchored to one project
- attached to multiple projects
- utility automation-backed for Nucleus-owned playbooks and background runs

The working directory should come from the workspace and active-project model, not from arbitrary per-session free text.

The expected behavior is:

- no project active: use workspace scratch or workspace root rules
- one project active: use that project root
- multiple projects active: use a primary project as the anchor and attach the others as additional context

## Routing

Routing belongs to Nucleus.

The workspace owns:

- a default main model target
- a default utility model target
- named profiles that package model and provider choices for common work modes
- default Utility Worker run-budget limits

Clients should present those choices cleanly, but Nucleus remains the authority.

## Prompt Context

Prompt context should layer like this:

1. Nucleus system instructions
2. committed public repo context
3. local private operator context
4. project-specific context
5. session-specific user prompt

That keeps product truth stable while still allowing local overrides and project-specific guidance.
