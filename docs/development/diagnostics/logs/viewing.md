# Viewing logs

Logs are primarily consumed in either an interactive ("online") context with a live device, or in an
"offline" context with logs collected from past execution of a device.

## Ordering

All logs have a timestamp attached, which is read from the [monotonic clock] when recording the
message. There are many ways that a `LogSink` can receive messages in a different order than
indicated by their timestamps.

## Online

Because there are two buffers that store logs, there are two main ways to view them when you have a
live device. For more information about where logs are stored on-device, see [Concepts: Storage].

### syslog and kernel log

During development, running [`ffx log`] is a good default to see all logs, including those
[forwarded from the klog].

For scenarios when there's only serial console access, no network or you are simply working on a
`bringup` build, it can be useful to run [`log_listener`] in the serial console. This command uses
the same CLI as `ffx log`. If you are working with `log_listener`, any example that uses `ffx log`
can be replaced with `log_listener`.

`ffx log` receives logs through the [`fuchsia.diagnostics.ArchiveAccessor`] protocol.

Additionally, some logs from syslog are printed to the serial console. By default, this includes the
all components under the `bootstrap` realm (for example, drivers and `driver_manager`). Additional
components may be added through the [Diagnostics assembly configuration][assembly-diagnostics].

#### Format

By default, [`ffx log`] emits lines in this format:

```
[seconds][pid][tid][tags] LEVEL: message
```

The timestamp is from the monotonic clock by default, and it is formatted with microsecond
granularity.

If the message "something happened" is written at WARN level by my-component from process=1902
and thread=1904 at time=278.14, the default output would be:

```
[278.14][1902][1904][my-component] WARN: something happened
```

`ffx log` has `--hide_metadata` and `--pretty` flags that reduces the printed metadata,
and color codes log lines by severity, respectively. With these flags, some metadata is hidden
(PID, TID, etc.) while others are trimmed down (timestamp, severity).

For example, if the message "something happened" is printed at WARN level by my-component at
time=278.14, the pretty output will look like:

```
[278.14][my-component][W] something happened
```

With a running device available, run `ffx log --help` to see the options for modifying the output
format.

#### `fx test`

Under the hood, `fx test` calls `run-test-suite`, which collects isolated `stdout`, `stderr`, and
`LogSink` connections from test components, printing the output inline and preventing them showing
up in the global log buffers.

For tests that are not yet components no interception of logs is performed.

### kernel log only

The klog is [printed over the kernel console] and serial.

It's also [forwarded over UDP by netsvc], which is what's printed when you run `fx klog`. Running
`fx klog` in a background terminal can be a good way to capture logs if your SSH session fails, or
as a backup if there are other issues with running `ffx log`.

If neither of the above are options, you can also use [`dlog`] from a device shell directly to dump
the kernel debug logs.

#### Format

The kernel log's dumper emits lines in the format:

```
[timestamp] pid:tid> message
```

The timestamp is from the monotonic clock. It is formatted with 5 digits (leading zeroes) for
seconds and three digits for milliseconds (trailing zeroes).

Process and thread koids are written with 5 digits each (leading zeroes).

If the message "something happened" is written from process=1902 and thread=1904 at time=278.14, the
resulting output would be:

```
[00278.140] 01902:01904> something happened
```

The `fx pretty_serial` command can be used to reduce the metadata printed by klog and color code
log lines by severity. With this command, some metadata is hidden (PID, TID, filenames, etc.)
while others are trimmed down (timestamp, severity).

Serial output should be piped in from the emulator or from other sources:

```
ffx emu start --console | ffx debug symbolize
```

For example, if the message "something happened" is printed to klog at WARN level by
my-component at time=278.14, the pretty output will look like:

```
[278.14][my-component][W] something happened
```

For example, if the message "something happened" is printed to klog by an unknown component with
unknown severity at time=278.14, the pretty output will look like:

```
[278.14] something happened
```

#### Dynamically setting minimum log severity

By default, components that integrate with [`fuchsia.logger.LogSink`] through Fuchsia's logging
libraries, support the configuration of their minimum log severity at
runtime.

You can do this by passing `--set-severity` to `ffx log`. The `--set-severity` option accepts a
string argument in the format of `<component_query>#<SEVERITY>`, where
`<component_query>` is a string that will fuzzy match a component moniker or URL
and `<severity>` is one of `TRACE`, `DEBUG`,`INFO`,`WARN`,`ERROR`,`FATAL`.

For example, if you run `ffx log --set-severity netstack#DEBUG`, the `core/network/netstack`
component starts emitting `DEBUG` logs and the `ffx log` output prints a log stream containing
`DEBUG` logs for `netstack`. When the command is stopped (for example through Ctrl-C) the
component goes back to emitting logs with its default minimum severity (typically `INFO`).

This functionality is also supported for tests. For more information, see
[Set the minimum log severity][test-dynamic-severity]).


## Offline: CQ/CI/LUCI

When running tests, a [Swarming] bot invokes [botanist], which collects several output streams to be
presented in the web UI. The `stdout` & `stderr` of botanist are what's presented in the "swarming task
UI".

For individual test executables botanist uses [testrunner] lib and collects that output separately.
It is this output that can be seen after a failing test, with a link named `stdio`. Most tests that
testrunner invokes run `run-test-suite` via SSH to the target device. This collects the
stdout, stderr, and logs from the test environment and prints them inline.

### syslog.txt

Botanist runs `ffx log` on the target device and saves that output to syslog.txt.

### infra_and_test_std_and_klog.txt

This log includes the stdout and stderr of the command run by the [Swarming] task.
Normally this includes the following notable items, all interleaved:

* [botanist]'s log messages
* kernel log from netsvc (equivalent to `fx klog`)
* `stdout` and `stderr` of the tests run by testrunner

This aggregate log is run through the equivalent of `ffx debug symbolize` before upload.

### serial_log.txt

This log includes serial logs from a device.

### triage_output.txt

This includes the results of running the [triage tool] on a snapshot collected
from a device.

### summary.json

A structured output summary for test execution.

### $debug

Debug logs emitted during infra's recipe step execution.

### $execution details

The details of a recipe step, including the command run and environmental details.
This log is often helpful for reproducing the recipe step locally.

[assembly-diagnostics]: /reference/assembly/DiagnosticsConfig/index.md
[monotonic clock]: /reference/syscalls/clock_get_monotonic.md
[Concepts: Storage]: /docs/concepts/components/diagnostics/README.md#log_storage
[forwarded from the klog]: /docs/development/diagnostics/logs/recording.md#forwarding-klog-to-syslog
[`fuchsia.diagnostics.ArchiveAccessor`]: /reference/fidl/fuchsia.diagnostics#ArchiveAccessor
[`log_listener`]: /src/diagnostics/log_listener/README.md
[`fuchsia.logger.LogSink`]: /reference/fidl/fuchsia.logger#LogSink
[printed over the kernel console]: /zircon/kernel/lib/debuglog/debuglog.cc
[forwarded over UDP by netsvc]: /src/bringup/bin/netsvc/debuglog.cc
[`dlog`]: /src/bringup/bin/dlog/README.md
[`ffx log`]: /reference/tools/sdk/ffx#ffx_log
[botanist]: /tools/botanist/cmd/main.go
[testrunner]: /tools/testing/testrunner/lib.go
[test-dynamic-severity]: /docs/development/testing/run_fuchsia_tests.md#set_the_minimum_log_severity
[triage tool]: /docs/development/diagnostics/triage/README.md
[Swarming]: https://chromium.googlesource.com/infra/luci/luci-py/+/HEAD/appengine/swarming/doc/README.md
