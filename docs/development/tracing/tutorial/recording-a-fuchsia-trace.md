# Recording a Fuchsia trace

This document describes how to record a trace with Fuchsia's
[tracing system](/docs/concepts/kernel/tracing-system.md).

## Prerequisites

Before you attempt to record a trace, make sure you have
done the following:

* Registered your component as a trace provider. See
  [Registering a trace provider](/docs/development/tracing/tutorial/registering-a-trace-provider.md).
* Added tracing in your code. See
  [Adding tracing in your code](/docs/development/tracing/tutorial/adding-tracing-in-code.md).
* Included the `tools` to your Fuchsia build. The `core` product and most other
  products include `tools` by default. If your build configuration does not
  include `tools` bundle by default, then you can manually add it with `fx set`:

  <pre class="prettyprint">
  <code class="devsite-terminal">fx set <var>PRODUCT</var>.<var>BOARD</var> --with-base '//bundles/tools'</code>
  </pre>

## Use the utilities

Traces are recorded with the `trace` utility on a Fuchsia target.
The [`ffx trace start`][ffx-trace] command, which you can run from
your development host, calls the `trace` utility on your Fuchsia target.

You can record a trace from your Fuchsia target from your development host
or directly from the Fuchsia target.

* [From a development host](#from-a-development-host)
* [From a Fuchsia target](#from-a-fuchsia-target)

### From a development host {#from-a-development-host}

To record a trace for a Fuchsia target from a development host,
run the following command:

```posix-terminal
ffx trace start [--duration <SECONDS>]
```

`ffx trace start` does the following:

 * Starts a trace on the Fuchsia target with the default options.
 * Runs the tracing until the `Enter` key is pressed, or the duration is
   reached if provided.
 * Prints the trace results from the Fuchsia target device to an output file
   on your development host.

#### Categories and category groups

You can control what kinds of data is collected during the trace session by specifying
a `--categories` argument to `ffx trace start`.  The full list of categories can be
accessed by running the following command:

```posix-terminal
ffx trace list-categories
```

`ffx trace start` also supports "category groups" - predefined lists of categories which
are notated with a `#` prefix. For example, `#chrome_nav`, which expands to
`loading,net,netlog,neavigation,browser`, can be used to quickly specify all the
events relating to resource loading and page navigation. The default categories
are also represented as a category group called `#default`. Custom category groups may be set
using `ffx config set`. To see the full list of category groups, run:

```posix-terminal
ffx config get -s all trace.category_groups
```

If you would like to define a custom category group for an often-used set of categories,
you can do so by running a command similar to the one below, which defines a category group
called `#audiovisual`:

```posix-terminal
ffx config set trace.category_groups.audiovisual '["audio", "gfx"]'
```

For a complete list of the `ffx trace start` options, run `ffx trace start --help`.

Once you have the trace output file, you can
[convert and analyze that trace file](/docs/development/tracing/tutorial/converting-visualizing-a-trace.md).

#### Tracing Chrome and WebEngine events

Tracing information can be collected from Chrome and WebEngine by calling `ffx trace start`
with a list of Chrome-specific categories or category groups. You may also specify categories
outside of Chrome as well. The list of Chrome category groups are:

 * `#chrome_input`: Input handling events.
 * `#chrome_ipc_flows`: Mojo IPC routing events.
 * `#chrome_js_exec`: JavaScript (V8) events.
 * `#chrome_nav`: Resource loading, page navigation, browser events.
 * `#chrome_task_sched`: Asynchronous task scheduling/dispatch events.
 * `#chrome_ui_render`: Chrome UI (browser UX, browser widgets, compositor, GPU) events.
 * `#chrome_web_content_render`: Content rendering (Blink, compositor, GPU) events.

All Chrome category groups include the categories `toplevel` and `toplevel.flow` which cover
a variety of basic Chrome events such as async task scheduling.

You may also combine Chrome categories with Fuchsia categories. For example, if you would like to
see trace data for Chrome content rendering alongside Zircon kernel scheduler activity, you can run
the following command:

```posix-terminal
ffx trace start --categories kernel:sched,#chrome_web_content_render
```

### From a Fuchsia target {#from-a-fuchsia-target}

To record a trace directly from a Fuchsia target, run the following
command in a shell on your target:

<pre class="prettyprint">
<code class="devsite-terminal">trace record</code>
</pre>

This saves your trace in `/data/trace.json` on your Fuchsia target by default.
For more information on the `trace` utility, run `trace --help` at a Fuchsia shell.

Once you have the trace output file, you can
[convert and analyze that trace file](/docs/development/tracing/tutorial/converting-visualizing-a-trace.md).

<!-- Reference links -->

[ffx-trace]: https://fuchsia.dev/reference/tools/sdk/ffx#trace
