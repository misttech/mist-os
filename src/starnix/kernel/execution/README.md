# Starnix Execution

The execution module handles executing Linux tasks inside Starnix. It uses Zircon's "restricted
mode" to execute Linux tasks efficiently. This crate also provides a
bridge between Fuchsia's execution model and Linux tasks.

## Container logic

Linux tasks run inside a "container" which provides a shared environment similar to a virtual machine
or Docker container. A container is associated with a single Starnix Kernel object, a root system
image and init configuration.

The code in `container.rs` deals with these concerns.

## Executor

The executor is responsible for transferring control in and out of Linux logic and coordinating state
changes with Zircon. To achieve this, the executor sets up Zircon objects (processes, vmars, etc) to
contain and execute Linux logic.

The restricted executor takes advantage of Zircon's restricted execution mode feature
(https://fuchsia.dev/fuchsia-src/reference/syscalls#restricted_mode_work_in_progress) to
efficiently handle syscalls from Linux. Specifically:

1. A Zircon process is created for each Linux thread group.

2. A Zircon thread is created for each Linux thread.

3. The process' address space is divided in to two ranges: a restricted range covering
   the lower half of the userspace range and a shared range covering the upper half.

   Linux programs have access to the restricted range. The shared range is the same for
   every process in the same container and is used to manage Starnix state across the container.

4. Threads in this process can be executing in either "restricted mode" or "normal mode". Threads
   begin execution in normal mode with the shared range accessible. Starnix enters restricted mode
   by issuing a `zx_restricted_enter()` syscall which makes the restricted range accessible and the
   shared range inaccessible.

5. Linux code runs in restricted mode until it issues a `syscall` instruction or generates an exception.
   On a syscall from restricted mode, Zircon places the thread back in normal mode and returns from the
   `zx_restricted_enter()` syscall. The restricted executor then decodes and dispatches the syscall.
   The same pattern applies to other exits from restricted mode. For example, see the next item...

6. On an exception, Zircon places the thread back in normal mode, and returns from the `zx_restricted_enter()`
   syscall. The restricted executor then executes Starnix code to handle the Zircon exception.
   Some exceptions are handled internally within Starnix by adjusting the memory mapping or other state.
   Other exceptions generate Linux signals which are delivered according to the task's signal disposition.

7. The executor exports data about the state of the Linux address space to Fuchsia-aware debugging
   tools such as crashsvc and zxdb.

This diagram shows the process, address space, and thread relationships for a Linux thread group
containing 2 threads running in the restricted executor:

```
                  Zircon process

                  Restricted vmar

                  0x...020000 0x4000...100000

                 +----------------------+
                 |Linux thread group    |
                 |                      |
                 | Thread 1 |  Thread 2 |
                 |          |           |
restricted_enter |          |           |
+----------------+---->     |  fault    |    ZX_EXCP_...
|                |          |     ------+-------------------+
|                | syscall  |           |                   |
|            +---+-----     |           |                   |
|            |   |          |           |                   |
|            |   |          |           |                   |
|            |   +----------+-----------+                   |
|            |                                              |
|            |   Shared (aka root) vmar                     |
|            |                                              |
|            |   0x4000...100000  0x8000...1000             |
|            |                                              |
|            |   +---------------------+                    |
|            |   | Restricted executor |                    |
|            |   |                     |                    |
|            |   | Thread 1 | Thread 2 |                    |
|            |   |          |          |                    |
|            |   |          |          |                    |
+------------+---+-----     |          |                    |
             |   |          |   <------+--------------------+
             +---+---->     |          |
                 |          |          |
                 |          |          |
                 |          |          |
                 +----------+----------+
```

The shared portion of the address space is shared between all Linux thread groups in the same
container. This allows Starnix to access information about any thread group in the container when handling
a system call or exception.