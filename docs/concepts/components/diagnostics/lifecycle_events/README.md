# Lifecycle events

The [Archivist][archivist] consumes lifecycle events to ingest diagnostics data. This document
explains what these events are.

## Archivist consumption of lifecycle events {#archivist-consumption}

The archivist ingests events from the component framework.
The following diagram shows a very high level overview of the three lifecycle events
(started, directory_ready and stopped) the archivist is interested in.

![Figure: Flow of lifecycle events under component manager](component_manager_lifecycle_flow.png)

The archivist consumes the following lifecycle events under component manager through
[`fuchsia.component.EventStream`][event_stream]:

- **Directory ready**: The archivist listens for directory ready of the `out/diagnostics`
  directory. When the component starts serving this directory, the component manager sends this
  event to the Archivist.
- **Capability requested**: The archivist receives `Capability requested` events for connections to
  `fuchsia.logger.LogSink` and `fuchsia.inspect.InspectSink` which allows it to attribute Inspect
  and logs.

## Related docs

- [Event stream capabilities][event_capabilities]
- [Inspect discovery and hosting - Archivist section][inspect_discovery_hosting]


[archivist]: /docs/reference/diagnostics/inspect/tree.md#archivist
[event_source]: https://fuchsia.dev/reference/fidl/fuchsia.component#EventStream
[event_capabilities]: /docs/concepts/components/v2/capabilities/event.md
[inspect_discovery_hosting]: /docs/reference/diagnostics/inspect/tree.md#archivist
[runner]: /docs/glossary#runner
