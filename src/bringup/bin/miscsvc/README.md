# Miscellaneous Service

This component provides a home for some low-level services used by drivers and
other bootstrap components. These services previously were hosted by svchost,
but has proved problematic, so they have been isolated into a separate process.

Services implemented by miscsvc are exposed to svchost, which exposes it out
further to other parts of the system.

Once component manager lands in bootfs, and adequate build support arrives,
these services can be rewritten as components.
