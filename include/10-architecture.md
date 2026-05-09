# Core Boundary

Nucleus is the system of record.

Clients may read, steer, and subscribe, but they must not invent durable backend truth or bypass Nucleus-owned operations.

Installed Nucleus should run as one local service process that serves the built web UI and exposes one authenticated API surface.

Prompt execution should enter the Nucleus-owned Utility Worker/job path for text and image turns alike. Uploaded images remain scoped turn attachments, not repo files. If the selected runtime cannot support vision with workspace actions, Nucleus should degrade explicitly instead of silently dropping workspace access.

User-facing activity should use product language: Nucleus, Utility Worker, Utility Subworker, and Action or concrete action labels. Lower-level daemon/tool-call terms belong only in architecture and developer docs when they are technically necessary.
