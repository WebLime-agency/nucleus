# Skills

Nucleus skills are installed instruction packages that can contribute prompt layers to a session turn. The durable content unit is a `skill_package`; a `skill_installation` controls scope and enablement; a `skill_manifest` controls matching metadata such as title, triggers, activation mode, include paths, and project filters.

## Activation modes

- `always`: active whenever the skill is enabled and an enabled installation applies to the session.
- `auto`: active when the current user prompt matches an explicit trigger, the skill id, normalized id tokens, skill title, package name, or package manifest title/name/description metadata.
- `manual`: not selected through loose metadata relevance, but an exact user mention of the skill title or id activates it for that turn.

This means an installed skill with id `emdash-site-architecture` and title `EmDash Site Architecture` activates when the user says either `Use emdash-site-architecture` or `I'd like to work on EmDash Site Architecture`, even if `triggers` is empty.

## Prompt content sources

When a skill activates, Nucleus compiles prompt layers from:

1. installed package `instructions` (first-class source of truth),
2. manifest `instructions`, if present and not duplicated, and
3. allowed include files, if present and not duplicated.

Include files are restricted for safety. Nucleus allows workspace-relative include files under the workspace root and Nucleus-owned skill files shaped like `<state-dir>/skills/<skill-id>/SKILL.md` (for example `/home/eba/.nucleus-eba/skills/emdash-site-architecture/SKILL.md`). Arbitrary absolute paths are rejected.

## Installation scope

Only enabled installations that apply to the current session are eligible:

- workspace installations apply to the workspace,
- project installations apply when the session includes that project,
- session installations apply only to the exact session.

Installed package mapping accepts both `manifest_json.manifest_id` and `manifest_json.id`; Nucleus package ids of the form `nucleus.<skill-id>` are also supported for backward compatibility.

## Diagnostics

Compiled turn debug summaries include `skill_diagnostics`, which records why each considered skill was selected or skipped, plus which prompt source was loaded. Typical reasons include:

- `disabled`
- `no enabled installation for this session`
- `project filter mismatch`
- `activation mode did not match trigger/title/id metadata`
- `package instructions loaded`
- `include rejected or missing`

These diagnostics are intended to make it obvious whether an installed skill was selected and whether its instructions reached the model prompt.
