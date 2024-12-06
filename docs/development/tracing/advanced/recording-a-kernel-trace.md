# Recording a kernel trace

The kernel traces various actions by writing records to an internal buffer,
which can later be retrieved, printed, and viewed.

## Kernel trace format

The kernel trace format uses [FXT](/docs/reference/tracing/trace-format.md)

## Controlling what to trace

Control of what to trace is provided by a kernel command-line parameter
`ktrace.grpmask`. The value is specified as 0xNNN and is a bitmask
of tracing groups to enable. See the **KTRACE\_GRP\_\** values in
`system/ulib/zircon-internal/include/lib/zircon-internal/ktrace.h`.
The default is 0xfff, which traces everything.

What to trace can also be controlled by the `ktrace` command-line utility,
described below.

## Trace buffer size

The size of the trace buffer is fixed at boot time and is controlled by
the `ktrace.bufsize` kernel command-line parameter. Its value is the
buffer size in megabytes. The default is 32MB.

## ktrace command-line utility

Kernel tracing may be controlled with the `ktrace` command-line utility.

```
$ ktrace --help
Usage: ktrace [options] <control>
Where <control> is one of:
  start <group_mask>  - start tracing
  stop                - stop tracing
  rewind              - rewind trace buffer
  written             - print bytes written to trace buffer
    Note: This value doesn't reset on "rewind". Instead, the rewind
    takes effect on the next "start".
  save <path>         - save contents of trace buffer to <path>

Options:
  --help  - Duh.
```

## Viewing kernel trace

Traces can be uploaded to ui.perfetto.dev to be viewed. Alternatively, the host tool `trace2json`
can be used to export a trace to a more readable json output.

Example:

First collect the trace on the target:

```
$ ktrace start 0xfff
... do something ...
$ ktrace stop
$ ktrace save /tmp/save.ktrace
```

Then copy the file to the development host, and export it:

```
host$ fx cp --to-host /tmp/save.ktrace save.fxt
host$ fx trace2json < save.fxt > save.json
```

The output can be quite voluminous, thus it's recommended to send it to a
file and then view it in your editor or whatever.

## Use with Fuchsia Tracing

Fuchsia's tracing system supports collecting kernel trace records through
the `ktrace_provider` trace provider.
For documentation of Fuchsia's tracing system see the documentation in
[Fuchsia tracing system](/docs/concepts/kernel/tracing-system.md).

## More information

More information on `ktrace` can be found in the
[full list of kernel command line parameters](/docs/reference/kernel/kernel_cmdline.md).
