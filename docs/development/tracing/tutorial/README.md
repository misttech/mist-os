# Tutorial on Fuchsia tracing

This tutorial walks through how to register a Fuchsia component to paticipate
in tracing and add tracing events in the component's code. Once a component
is configured for tracing, you can use the [`ffx trace start`][record-traces]
command to record a trace on a Fuchsia device and visualize the trace results
for analysis.

The [Fuchsia tracing system][fuchsia-tracing-system] provides a mechanism for
collecting and visualizing diagnostic tracing information from user space processes
and the Zircon kernel on a Fuchsia device. The Fuchsia tracing system is made up of
a trace manager, a memory buffer, and one or more trace providers. A trace provider
is a component that generates trace data as it runs on the device.

Many existing Fuchsia components are already registered as trace providers, whose
trace data often provide a sufficient overview of the system. For this reason,
if you only need to record a general trace (for instance, to include details in
a bug report), you may skip directly to the
[Record and visualize a trace][record-and-visualize-a-trace] step. However,
if you want to collect additional, customized trace events from a specific
component, complete the tutorial from the start.

The steps are:

1. [Register a trace provider][register-a-trace-provider].
2. [Add tracing in your code][add-tracing-in-your-code].
3. [Record and visualize a trace][record-and-visualize-a-trace].

<!-- Reference links -->

[record-traces]: /docs/development/sdk/ffx/record-traces.md
[fuchsia-tracing-system]: /docs/concepts/kernel/tracing-system.md
[register-a-trace-provider]: /docs/development/tracing/tutorial/register-a-trace-provider.md
[add-tracing-in-your-code]: /docs/development/tracing/tutorial/add-tracing-in-code.md
[record-and-visualize-a-trace]: /docs/development/tracing/tutorial/record-and-visualize-a-trace.md
