# Record traces for performance analysis

The [`ffx trace`][ffx-trace] commands can record diagnostic trace data from
a Fuchsia device. This data can be used to measure and debug performance
issues, understand interactions between threads, processes, and components,
and visualize the system as a whole.

## Concepts

The [Fuchsia tracing system][fuchsia-tracing-system] provides a mechanism
to collect, aggregate, and visualize diagnostic tracing information from
user space processes and the Zircon kernel on a Fuchsia device. The tracing
system is made up of a trace manager, a memory buffer, and one or more trace
providers. A [trace provider][trace-providers] is a component that generates
trace data on the device, and a system can have
[many trace providers](#view-trace-providers). (To register your component as
a trace provider, see [Register a trace provider][register-a-trace-provider].)

The `ffx trace start` command stores the output of tracing as a  [`.fxt`][fxt]
file on the host machine. You can open this file on the
[Perfetto viewer][perfetto-viewer]{:.external} to visualize the trace results
for performance analysis. (For more information on Perfetto, see this
[Perfetto documentation][perfetto-docs]{:.external} site.)

By default, the `ffx trace start` command attempts to collect trace data from
a predefined set of trace categories (run `ffx trace start --help` to see the
default categories). However, `ffx trace start` also allows you to
[select trace categories](#view-trace-categories) for collecting trace data.

Only one trace session can be running on a Fuchsia device at a time, and only
a single trace can be recorded on an output file. In the examples below, all
output files default to `trace.fxt` in the directory where `ffx trace` is run,
and all target devices default to an
[available Fuchsia device][view-device-information] connected to the host
machine at the time.

## Run a trace interactively {:#run-a-trace-interactively}

With an interactive trace, you can press the `Enter` key to decide when to
end the tracing in real time. However, if the `--duration` flag is specified,
the tracing stops automatically when the duration is reached. (For more
information on how results are stored in the buffer while the tracing is
running, see [Buffering mode options](#buffering-mode-options) in Appendices.)

To start an interactive trace, run the following command:

```posix-terminal
ffx trace start
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start
Tracing started successfully on "fuchsia-5254-0063-5e7a".
Writing to /Users/alice/trace.fxt
Press <enter> to stop trace.

```

To stop the trace, press the `Enter` key.

The command exits with output similar to the following:

```none {:.devsite-disable-click-to-copy}
Shutting down recording and writing to file.
Tracing stopped successfully on "fuchsia-5254-0063-5e7a".
Results written to /Users/alice/trace.fxt
Upload to https://ui.perfetto.dev/#!/  to view.
```

To analyze the results collected from this run, see
[Visualize trace results](#visualize-trace-results).

## Run a trace in the background {:#run-a-trace-in-the-background}

A background trace runs indefinitely, as long as a duration is not
specified. To stop a trace running in the background, you need to run
[`ffx trace stop`](#stop-a-trace). (For more information on how results
are stored in the buffer while the tracing is running, see
[Buffering mode options](#buffering-mode-options) in Appendices.)

To start a background trace, run the following command:

```posix-terminal
ffx trace start --background
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --background
Tracing started successfully on "fuchsia-5254-0063-5e7a".
Writing to /Users/alice/trace.fxt
Current tracing status:
- fuchsia-5254-0063-5e7a:
  - Output file: /Users/alice/trace.fxt
  - Duration: indefinite
  - Config:
    - Categories:
      - app,audio,benchmark,blobfs,gfx,input,kernel:meta
```

To stop this tracing, see [Stop a trace](#stop-a-trace).

### Run a trace in the background with a timer {:#run-a-trace-in-the-background-with-a-timer}

Similar to the interactive trace, you can run the tracing in the
background and set it to stop after a specific time duration.

Note: Unlike the interactive trace, a background trace can be given
a duration in fractional seconds (for example, `–duration 1.5` runs
for 1.5 seconds).

To start a background trace with a timer, run the following command:

```posix-terminal
ffx trace start --background --duration <SECONDS>
```

Replace `SECONDS` with a target duration in seconds, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --background --duration 20
```

To stop this tracing manually, see [Stop a trace](#stop-a-trace).

### Run a trace in the background with a trigger {:#run-a-trace-in-the-background-with-a-trigger}

If a trace is run with a trigger, the tracing stops when the specified
event is detected.

Note: Traces with triggers can only be run in the background.
At the moment, the only available action is `terminate`.

To run a trace with a trigger, run the following command:

```posix-terminal
ffx trace start --background --trigger <TRIGGER>
```

Replace `TRIGGER` with an action using the syntax `alert:action`,
for example:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --background --trigger "myexample:terminate"
```

To stop this tracing manually, see [Stop a trace](#stop-a-trace).

### Check the status of traces in the background {:#check-the-status-of-traces-in-the-background}

To check the status of background traces, run the following command:

```posix-terminal
ffx trace status
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace status
- fuchsia-5254-0063-5e7a:
  - Output file: /Users/alice/trace.fxt
  - Duration: indefinite
  - Config:
    - Categories:
      - app,audio,benchmark,blobfs,gfx,input,kernel:meta
```

If there are no traces in the background, the command prints
the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace status
No active traces running.
```

### Stop a trace {:#stop-a-trace}

The `ffx trace stop` command stops a trace running in the background.

Note: To see all traces running in the background, run
[`ffx trace status`](#check-the-status-of-traces-in-the-background).

To stop a trace, run the following command:

```posix-terminal
ffx trace stop [--output <FILE>]
```

By default, the command stops a trace that matches the default target
device. However, you can also select which trace to stop by using the
`–output` flag, which then stops the trace that is associated with the
output file.

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace stop
Tracing stopped successfully on "fuchsia-5254-0063-5e7a".
Results written to /Users/alice/trace.fxt
Upload to https://ui.perfetto.dev/#!/ to view.
```

To analyze the results collected from this run, see
[Visualize trace results](#visualize-trace-results).

## Visualize trace results {:#visualize-trace-results}

Once a trace is finished and a `.fxt` file is created, open the file
on the Perfetto viewer to visualize the trace results.

Do the following:

1. Visit the [Perfetto viewer][perfetto-viewer]{:.external} site on
   a web browser.
2. Click **Open trace file** on the navigation bar.
3. Select your `trace.fxt` file from the host machine.

## View trace categories {:#view-trace-categories}

The `ffx trace start` command allows you to select categories which
are used to collect trace data, for example:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --categories "kernel,kernel:arch"
```

To see all available trace categories on a Fuchsia device,
run the following command:

```posix-terminal
ffx trace list-categories
```
This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace list-categories
Known Categories:
- app - Generic application traces
- benchmark - Benchmark traces
- cpu - several, run xyz for the list
- gfx - Graphics & Compositor
- input - Input system
- kernel - All kernel trace events
- kernel:arch - Kernel arch events

Default Categories:
- app
- audio
- benchmark
- blobfs
```

For more information on categories, see
[Categories and category groups](#categories-and-category-groups)
in Appendices.

## View trace providers {:#view-trace-providers}

To see all available [trace providers][trace-providers] on a Fuchsia
device, run the following command:

```posix-terminal
ffx trace list-providers
```

This command prints output similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx trace list-providers
Trace providers:
- ktrace_provider
```

## Appendices

### Categories and category groups {:#categories-and-category-groups}

The `ffx trace start` command supports special syntax to provide more
precise control over enabling or disabling specific categories:

- A trailing asterisk (`*`) performs a prefix match.

  For example, `kernel*` enables `kernel:meta, kernel:sched`.

- A slash (`/`) limits categories to a specific trace provider.

  For example, `archivist.cm/packet` enables the packet category for
  only the `archivist` trace provider.

`ffx trace start` also supports category groups, which are predefined
lists of categories notated with the `#` prefix. For example,
`#chrome_nav` (which expands to `loading`, `net`, `netlog`, `navigation`,
and `browser`) is used to specify all the events related to resource
loading and page navigation, and `#default` represents a group of the
default categories.

When running `ffx trace start`, category groups can be specified along
with categories. For example, the command below enables all the default
categories and the `my_cat` category:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --categories #default,my_cat
```

To see the full list of category groups on your host machine, run the
following command:

```posix-terminal
ffx config get -s all trace.category_groups
```

Custom category groups can be set using the `ffx config set` command.
If you want to define a custom category group for a set of
frequently-used categories, run a command similar to the following:

```none {:.devsite-disable-click-to-copy}
$ ffx config set trace.category_groups.audiovisual '["audio", "gfx"]'
```

The example command above defines a new custom category group called
`#audiovisual`.

### Chrome and WebEngine category groups {:#chrome-and-webengine-category-groups}

Chrome-specific category groups enable trace information to be
collected from Chrome and WebEngine.

The following is the list of Chrome category groups:

- `#chrome_input`: Input handling events.
- `#chrome_ipc_flows`: Mojo IPC routing events.
- `#chrome_js_exec`: JavaScript (V8) events.
- `#chrome_nav`: Resource loading, page navigation, and browser
   events.
- `#chrome_task_sched`: Asynchronous task scheduling and dispatch
   events.
- `#chrome_ui_render`: Chrome UI events (browser UX, browser
   widgets, compositor, and GPU).
- `#chrome_web_content_render`: Content rendering events (Blink,
   compositor, and GPU).

All Chrome category groups include the `toplevel` and `toplevel.flow`
categories, which cover a variety of basic Chrome events such as
async task scheduling.

When specifying categories, you can combine Chrome categories with
Fuchsia categories. For example, the command below collects trace data
for Chrome content rendering alongside Zircon kernel scheduler activity:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --categories kernel:sched,#chrome_web_content_render
```

### Buffering mode options {:#buffering-mode-options}

The `--buffering-mode` flag allows you to decide what happens when
the buffer fills up.

The options are:

- `oneshot`: Write to the buffer until it is full, then ignore all
  additional trace events. (This is the default option.)

- `circular`: Write to the buffer until it is full, then replace
  old events with new events.

- `streaming`: Forward tracing events to the trace manager as they
  arrive.

  The `streaming` option provides additional buffer space with the
  trade-off of some overhead due to occasional IPCs for sending the
  events to the trace manager.

The command below runs a trace with the largest available buffer
size (64 MB) and overwrites old events with new ones:

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --buffer-size 64 --buffering-mode circular
```

Similar to the [interactive mode](#run-a-trace-interactively),
press the `Enter` key to stop the trace.

The command below overwrites old events with new ones in the
buffer and stops the trace
[when the event happens](#run-a-trace-in-the-background-with-a-trigger):

```none {:.devsite-disable-click-to-copy}
$ ffx trace start --buffer-size 64 --buffering-mode circular --trigger 'myexample:terminate'
```

For the command above to work, the traced code must already be set up
to trigger the event.

<!-- Reference links -->

[ffx-trace]: https://fuchsia.dev/reference/tools/sdk/ffx#trace
[fuchsia-tracing-system]: /docs/concepts/kernel/tracing-system.md
[register-a-trace-provider]: /docs/development/tracing/tutorial/register-a-trace-provider.md
[fxt]: /docs/reference/tracing/trace-format.md
[perfetto-viewer]: https://ui.perfetto.dev/#!/
[perfetto-docs]: https://perfetto.dev/docs/
[record-a-boot-trace]: /docs/development/tracing/advanced/recording-a-boot-trace.md
[record-a-cpu-trace]: /docs/development/tracing/advanced/recording-a-cpu-performance-trace.md
[trace-providers]: /docs/concepts/kernel/tracing-system.md#trace-providers
[view-device-information]: /docs/development/sdk/ffx/view-device-information.md
