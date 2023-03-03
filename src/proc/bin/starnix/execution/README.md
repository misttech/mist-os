# Starnix Execution

The execution module handles executing Linux tasks inside Starnix. It has support for two execution
modes (exception and restricted) as well as logic common to all modes. This crate also provides a
bridge between Fuchsia's execution model and Linux tasks.

## Common logic

Starnix supports running multiple Linux programs with different levels of sharing. The top of the
hierarchy is a "container" which provides a shared environment similar to a virtual machine or Docker
container. A container is associated with a single Starnix Kernel object, a root system image and init
configuration.

## Executors

Executors are responsible for transferring control in and out of Linux logic and coordinating state
changes with Zircon. Different executors set up Zircon objects (processes, vmars, etc) to contain
and execute Linux logic.

## Common executor logic

### Exception handling

Many exception types such as page faults are handled in a uniform way across executors. Some
exceptions are handled internally within Starnix by adjusting the memory mapping or other state.
Several exceptions generated Linux signals which are delivered according to the task's signal
disposition.

### Debugger protocol

The executors export data about the state of the Linux address space to Fuchsia-aware debugging tools such as crashsvc and zxdb.

## Executors

### Exception Executor

The exception executor runs Linux programs in an isolated Zircon process and handles system calls
and exceptions by registering a Zircon exception handler for that process. The process used to run
the Linux program is created without a Zircon vDSO, so all syscalls issued by the process result in
a `ZX_EXCP_POLICY_CODE_BAD_SYSCALL` exception. The exception executor examines the register and
memory state of the thread raising an exception to determine which Linux syscall was intended and
then dispatches that. Other exceptions such as page faults are handled in the same fashion as the
restricted executor.

The exception executor creates one Zircon process per Linux thread group. This process devotes the
full userspace address space to the Linux program.

This diagram shows the process, thread, and exception handler arrangement for a Linux thread group
containing 2 threads running in the restricted executor:

```
                               Exception handler
 Zircon process --------------------------------------------------------+
                                                                        |
 Root vmar                                                              |
                                                                        |
 0x0...020000   0x8000...1000                                           v
                                              starnix_runner.cm
+----------------------+                    +---------------------------------------------------------+
|Linux thread group    |                    |               Exception executor                        |
|                      |                    |                                                         |
| Thread 1 |  Thread 2 |                    |  ZX_EXCP_POLICY_BAD_SYSCALL    ZX_EXCP_....             |
|          |           |                    |                |                      |                 |
|          |           |                    |                v                      v                 |
|          |           |                    |    execute_syscall()            task.process_exception()|
|          |           |                    |                                                         |
|          |           |                    |                                                         |
|          |           |                    |                                                         |
|          |           |                    |                                                         |
|          |           |                    |                                                         |
+----------+-----------+                    +---------------------------------------------------------+
```

### Restricted Executor

The restricted executor takes advantage of Zircon's restricted execution mode feature
(https://fuchsia.dev/fuchsia-src/reference/syscalls#restricted_mode_work_in_progress) to more
efficiently handle syscalls from Linux. This executor configures the address space in a more
elaborate fashion compared to the exception executor. A Zircon process is created for each Linux
thread group. The process' address space is divided in to two ranges: a restricted range covering
the lower half of the userspace range and a shared range covering the upper half. Linux programs
have access to the restricted range. The shared range is the same for every process in the same
container and is used to manage Starnix state across the container. Threads in this process can be
executing in either "restricted mode" or "normal mode". Threads begin execution in normal mode with
the shared range accessible. Starnix enters restricted mode by issuing a `zx_restricted_enter()`
syscall which makes the restricted range accessible and the shared range inaccessible. Linux code
runs in restricted mode until it issues a `syscall` instruction or generates a fault. On a syscall
from restricted mode, Zircon places the thread back in normal mode returns from the
`zx_restricted_enter` syscall. The restricted executor then decodes and dispatches the syscall. The
restricted executor also creates a thread for each Linux thread which monitors the thread for other
Zircon exceptions. These exceptions are dispatched in a manner similar to the exception executor.

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
+----------------+---->     |  fault    |      ZX_EXCP_...
|                |          |     ------+------------------------+
|                | syscall  |           |                        |
|            +---+-----     |           |                        |
|            |   |          |           |                        |
|            |   |          |           |                        |
|            |   +----------+-----------+                        |
|            |                                                   |
|            |   Shared (aka root) vmar                          |
|            |                                                   |
|            |   0x4000...100000  0x8000...1000                  |
|            |                                                   |
|            |   +---------------------------------------------+ |
|            |   |          Restricted executor                | |
|            |   |                                             | |
|            |   | Thread 1 | Thread 1  | Thread 2 | Thread 2  | |
|            |   |          | exception |          | exception | |
|            |   |          | handler   |          | handler   | |
+------------+---+-----     | thread    |          | thread    | |
             |   |          |           |          |    <------+-+
             +---+---->     |           |          |           |
                 |          |           |          |           |
                 |          |           |          |           |
                 |          |           |          |           |
                 +----------+-----------+----------+-----------+
```

The shared portion of the address space is shared between all Linux thread groups in the same
container. This allows Starnix to access information about any thread group in the container when handling
a system call or exception.

## Future work

There is a proposal being developed to allow handling exceptions and restricted mode exits via
syscall from the same thread. This will allow removing the exception handler thread from the
restricted mode executor.

There is currently no way to cause a thread executing in restricted mode to exit restricted mode
unless it raises an exception such as a fault or a syscall. There is a proposal underway to add a
mechanism to "kick" such a thread and exit to normal mode. This will be useful to allow Starnix to
exit a thread group cleanly or deliver a signal asynchronously.
