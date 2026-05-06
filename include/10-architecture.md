# Core Boundary

The daemon is the system of record.

Clients may read, steer, and subscribe, but they must not invent durable backend truth or bypass daemon-owned operations.

Installed Nucleus should run as one daemon process that serves the built web UI and exposes one authenticated API surface.
