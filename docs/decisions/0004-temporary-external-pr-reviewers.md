# 0004: Temporary External PR Reviewers

## Status

Accepted

## Context

Nucleus should eventually review and act on repository events through daemon-owned automations. That keeps review policy, authorization, context, and audit trails inside the product model instead of scattering them across external workflow integrations.

That automation surface is not ready yet. Until it exists, PR review coverage is still valuable for day-to-day development. The repository already uses Codex review on demand, and maintainers want Claude review available as a temporary second reviewer for PRs and tagged issue or PR comments.

## Decision

Nucleus will allow a narrow public exception for external PR reviewer workflow names, triggers, and action references that must identify the reviewer service in GitHub Actions configuration.

This exception applies only to:

- `.github/workflows/claude.yml`
- `.github/workflows/claude-code-review.yml`
- maintainer comments or PR metadata needed to trigger and operate those workflows

The exception does not change the product direction: Nucleus remains responsible for owning long-term automations, agent routing, permissions, durable state, and auditability.

## Guardrails

- The workflows must use repository secrets for authentication and must not hardcode tokens.
- The automatic PR review workflow should run only for non-draft PRs from branches in this repository.
- Triggered assistant runs should remain explicitly gated by maintainer-visible `@claude` mentions or repository events.
- Workflow permissions should stay at the minimum needed for the integration.
- This exception should be removed when Nucleus-native automations can review PRs and respond to repository events with comparable reliability.

## Consequences

The repository temporarily exposes external reviewer names in public workflow configuration. That is acceptable while the workflows are treated as transitional infrastructure, not as durable product architecture.
