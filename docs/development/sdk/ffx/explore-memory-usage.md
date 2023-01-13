# Measure the memory usage on a device

The `ffx profile memory` command can measure the RAM (Random Access Memory) usage of
a Fuchsia system.

## Concepts

The `ffx profile memory` command evaluates how much memory is used by VMOs
([Virtual Memory Objects][vmo]) in a Fuchsia system. Unlike Linux's `ps` command,
this command evaluates all VMOs whether they are mapped or not.

Under the hood, the `ffx profile memory` command uses the `memory_monitor` component
to capture the memory information of all VMOs in the system.

In Fuchsia, memory can be shared between processes because multiple processes can have handles to the same VMO.
In some cases it is useful to distinguish between memory that is shared and not shared. `ffx profile memory` does this
by reporting the memory usage of a process in 3 distinct (but overlapping) categories: *Private*, *Scaled*, and *Total*.
* *Private* memory is the total size of VMOs exclusively retained (directly or indirectly via children VMOs) by the process.
* *Scaled* memory is the total size of VMOs retained (directly or indirectly via children VMOs) by several processes.
The cost of each VMO is shared evenly among all its retaining processes. For example, A 500 KiB shared between 5 processed will add 100 KiB to each of the 5 processes.
* *Total* memory is the total size of VMOs retained (exclusively or not, directly or not) by the process.

Some VMOs have names attached to them, and based on the name it's often possible to have an idea of what a VMO is used for.
For example, if a VMO's name starts with "scudo" it is likely used by the scudo allocator.
This allows `ffx profile memory` to categorize the VMOs of a given process into probable sources. The list of categories include:
* *[scudo]*: aggregates the VMOs used by scudo, fuchsia's default memory allocator.
* *[stacks]*: aggregates the VMOs used to store the stacks of the threads.
* *[blobs]*: aggregates the VMOs handed out by blobfs. That may include child VMOs that have been modified.
* *[relro]*: aggregates the VMOs containing the relocated read-only section of binaries.
* *[data]*: aggregates the VMOs containing the data segment of binaries.
* *[unnamed]*: aggregates the VMOs with empty names.
VMOs with names that do not fall in any of the built-in categories are displayed in their own categories.

## Measure the memory usage over a time interval {:#measure-the-memory-usage-over-a-time-interval}

To track the memory usage over a specific time interval, run the following command:

```posix-terminal
ffx profile memory --interval {{ '<var>' }}SECONDS{{ '</var>' }}
```

Replace <var>SECONDS</var> with a time interval in seconds.

The example command below checks the memory usage of the target Fuchsia device
every 3 seconds until the command is terminated (usually by pressing `CTRL+C`
in the terminal):

```none {:.devsite-disable-click-to-copy}
$ ffx profile memory --csv --interval 3
```

Notice that the example command prints the output in the CSV format (`--csv`).
For debugging purposes, to obtain the raw data exported by the `memory_monitor`
component, you can run the command with the `--debug-json` option.

<!-- Reference links -->

[vmo]: /docs/reference/kernel_objects/vm_object.md
