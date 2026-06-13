# 0005: OS-Scheduled Local Jobs

## Status

Accepted

## Context

Nucleus needs an operator surface for maintenance jobs that already belong to the host operating system. These jobs are useful to observe beside Nucleus-owned automations, but they have a different execution model: the OS scheduler owns when they fire.

If Nucleus inserted itself into the fire-time execution path, local maintenance would become less reliable. The jobs would stop being independent system maintenance and would instead depend on the daemon being online at exactly the scheduled moment.

## Decision

Nucleus may observe and control allowlisted OS-scheduled units, beginning with `systemd --user` timers on Linux.

The daemon may:

- read timer and triggered-service state from the OS
- read recent logs from the OS journal
- enable or disable allowlisted timers
- ask the OS to start the triggered service for a run-now handoff

The daemon must not:

- persist these units as Nucleus-scheduled records
- model these units as playbooks
- add these units to any Nucleus scheduler
- spawn the unit workload itself
- require a daemon callback for scheduled execution

The allowlist is runtime product configuration in `app_settings` under `system_jobs_unit_globs`. The default is empty, so installs that do not configure local jobs do no polling work for this surface.

## Consequences

- OS-scheduled maintenance continues to fire when Nucleus is down.
- The Automations tab can show both Nucleus-owned playbooks and OS-owned local jobs without blurring their execution boundaries.
- Run-now controls hand execution to the OS scheduler backend by starting the triggered service, not the timer and not an in-daemon job.
- Future scheduler backends can be added beside the Linux user-systemd backend while preserving the same data-plane boundary.
