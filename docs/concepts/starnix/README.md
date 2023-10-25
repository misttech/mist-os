# Starnix

Starnix is a [runner][starnix-runner] that allows unmodified Linux programs to run on
a Fuchsia system. Linux programs on Fuchsia are run in a user-space process whose
system interface is compatible with the Linux ABI (application binary interface).
Instead of using the Linux kernel to implement this interface, Fuchsia implements the
interface in a Fuchsia user-space program called Starnix. Starnix then serves as
a compatibility layer that translates requests (syscalls) from Linux programs to the
appropriate Fuchsia subsystems.

For more information on the design details of Starnix, see
[RFC-0082: Running unmodified Linux programs on Fuchsia][starnix-rfc].

For instructions on running Linux programs, tests, and interactive consoles,
see Starnix's [`README`][starnix-readme] file.

## Table of contents

- [Making Linux syscalls in Fuchsia][making-linux-syscalls]
- [Starnix container][starnix-container]
- [Architecture of the Starnix VFS][starnix-vfs]

<!-- Reference links -->

[starnix-runner]: /docs/concepts/components/v2/starnix.md
[starnix-rfc]: /docs/contribute/governance/rfcs/0082_starnix.md
[starnix-readme]: https://cs.opensource.google/fuchsia/fuchsia/+/main:/src/starnix/kernel/README.md
[making-linux-syscalls]: /docs//concepts/starnix/making-linux-syscalls-in-fuchsia.md
[starnix-container]: /docs/concepts/starnix/starnix-container.md
[starnix-vfs]: /docs/concepts/starnix/architecture-of-the-starnix-vfs.md
