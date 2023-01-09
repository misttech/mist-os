# Measure the memory usage on a device

The `ffx profile memory` command can measure the RAM (Random Access Memory) usage of
a Fuchsia system.

## Concepts

The `ffx profile memory` command evaluates how much memory is used by VMOs
([Virtual Memory Objects][vmo]) in a Fuchsia system. Unlike Linux's `ps` command,
this command evaluates all VMOs whether they are mapped or not.

Under the hood, the `ffx profile memory` command uses the `memory_monitor` component
to capture the memory information of all VMOs in the system.

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
