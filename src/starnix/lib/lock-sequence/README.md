# Lock ordering definition and enforcement

In a codebase containing multiple locks which are simultaneously held by a
single thread, acquiring them in different order could lead to deadlocks.
To prevent this, this crate makes it possible to statically define the order
in which the locks can be acquired, and then enforce it everywhere the locks
in question are used.

See [examples](src/lib.rs) for more details.
