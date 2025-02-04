# Use trace events

When you instrument a Fuchsia component as a tracing provider, you can create
and observe various types of events. This document explains the differences
between the types of events, the best use cases for each type of event, how to
use events in your components, and the various arguments that you can use in a
tracing event.

## Events

A Fuchsia event type can be any of the following:

Note: In the Fuchsia trace format a tracing event is known as an
[Event Record][trace-format-event-record].

* [Instant event](#instant-event)
  Marks a point of time that has a name, category, and optional arguments.
* [Duration event](#duration-event)
  Marks two points in time and can contain a name, category, and optional
  arguments.
* [Counter event](#counter-event)
  Captures numeric samples that are typically represented in Perfetto as a
  stacked area chart.
* [Flow event](#flow-event)
  Describes flow handoffs between threads and across processes.
* [Async event](#async-event)
  Allows specifying an `id` that links events across different threads together
  and places these events on a single track.

In its most basic form, a trace event has a category (the first parameter) and
a name (the second parameter).

A trace event can accept up to 15 [arguments](#arguments).

When you use trace events, it is important to understand how categories work.
The following are some key concepts related to categories:

* Categories allow you to group events so that you can enable or disable
  them together.
* Categories are typically short ASCII string literals of `[A-Za-z-_:]+` such
  as `gfx` or `audio`.
* Categories are global. Consider namespacing them to your component.
* Categories are not enabled by default. If you add a category, make sure that
  you specify it when you capture a trace with
  `--categories #default,your_category`.
* `ffx trace` supports prefix matching. If you run
  `ffx trace start --categories your_component::*`, this enables
  `your_component::category1`, `your_component::category2`, and any other
  associated categories.

When you use an event, make sure to include the relevant library in your code:

* {C }

  Note: The header file is defined in
  [//zircon/system/ulib/trace/include/lib/trace/internal/event_common.h](/zircon/system/ulib/trace/include/lib/trace/internal/event_common.h).
  See [Tracing: C and C++ macros][fuchsia-tracing-c-macros] for the reference
  page.

  ```c
  #include <lib/trace/event.h>
  ```

* {C++}

  Note: The header file is defined in
  [/zircon/system/ulib/trace/include/lib/trace/internal/event_common.h](/zircon/system/ulib/trace/include/lib/trace/internal/event_common.h).
  See [Tracing: C and C++ macros][fuchsia-tracing-c-macros] for the reference
  page.

  ```cpp
  #include <lib/trace/event.h>
  ```

* {Rust}

  Note: See the [rustdoc page][rustdoc-fuchsia_trace] for the `fuchsia_trace`
  crate reference page.

  ```rust
  use fuchsia_trace::{ArgValue, Scope, ...};
  ```

You can also see a [simple example][trace-provider-cpp-example] in C++ on using
a trace provider.

### Instant event {#instant-event}

This is the most basic of events. An instant event can mark a point of time that
has a name, category, and optional arguments.

Categories can optionally be registered, but most tracing providers do not.

Note: The trace scope is not used in Perfetto.

At the minimum, an instant trace has both a name and a category to differentiate
it:

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD);
  ```

* {C++}

  ```cpp
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD);
  ```

* {Rust}

  ```rust
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread);
  ```

You can also make an instant event that includes additional arguments:

Note: For more information on arguments, see [Arguments](#arguments).

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, {{ '<var>argument_1</var>' }}, {{ '<var>argument_1_value</var>' }}, {{ '<var>argument_2</var>' }}, "{{ '<var>argument_2_string</var>' }}");
  ```

* {C++}

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, {{ '<var>argument_1</var>' }}, {{ '<var>argument_1_value</var>' }}, {{ '<var>argument_2</var>' }}, "{{ '<var>argument_2_string</var>' }}");
  ```

* {Rust}

  ```rust
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, {{ '<var>argument_1</var>' }} => {{ '<var>argument_1_value</var>' }}, {{ '<var>argument_2</var>' }} => "{{ '<var>argument_2_string</var>' }}");
  ```

In Perfetto, an instant event is indicated by a small arrow as shown in this
screenshot.

Note: The instant event is shown by a black box in the screenshot.

![A screenshot of the Perfetto trace viewer displaying the trace of a process
that is executing on multiple CPUs (CPU 0, CPU 1, CPU 2, and CPU 3). The event
is emitted at the 00:00:00.194617950 and shows a duration of 0s as it is an
instant event. The process is called `example_trace_provider.cm`.](images/instant_event_perfetto.png "A view of an instant event from a trace using
the Perfetto Viewer"){: width="600"}

### Duration event {#duration-event}

A duration event marks two points in time and can contain a name, category,
and optional arguments. Duration events can also nest within another duration
event and stack on top of one another.

At a minimum a duration trace has both a name and a category to differentiate
it:

Note: Duration events must **begin** and **end** as last in, first out.

* {C }

  ```c
  TRACE_DURATION_BEGIN("{{ '<var>trace_category</var>' }}", "example_duration");
  // Do some work
  TRACE_DURATION_END("{{ '<var>trace_category</var>' }}", "example_duration");
  ```

* {C++}

  ```c
  TRACE_DURATION_BEGIN("{{ '<var>trace_category</var>' }}", "example_duration");
  // Do some work
  TRACE_DURATION_END("{{ '<var>trace_category</var>' }}", "example_duration");
  ```

* {Rust}

  ```rust
  duration_begin!(c"{{ '<var>trace_category</var>' }}", c"example_duration");
  // Do some work
  duration_end!(c"{{ '<var>trace_category</var>' }}", c"example_duration");
  ```

Alternatively, you can define a duration event and automatically close it when
it goes out of scope using RAII (Resource acquisition is initialization) as follows::

* {C }

  ```c
  {
    TRACE_DURATION("{{ '<var>trace_category</var>' }}", "example_duration_raii");
    // Do some work
    TRACE_DURATION(c"{{ '<var>trace_category</var>' }}", "nested duration", "{{ '<var>argument_3</var>' }}", {{ '<var>argument_3_value</var>' }}, "{{ '<var>argument_4</var>' }}", {{ '<var>argument_4_string</var>' }});
    // nested_duration closes due to RAII
    // trace_duration_raii closes due to RAII
  }
  ```

* {C++}

  ```cpp
  {
    TRACE_DURATION("{{ '<var>trace_category</var>' }}", "example_duration_raii");
    // Do some work
    TRACE_DURATION(c"{{ '<var>trace_category</var>' }}", "nested duration", "{{ '<var>argument_3</var>' }}", {{ '<var>argument_3_value</var>' }}, "{{ '<var>argument_4</var>' }}", {{ '<var>argument_4_string</var>' }});
    // nested_duration closes due to RAII
    // trace_duration_raii closes due to RAII
  }
  ```

* {Rust}

  ```rust
  {
    duration!(c"{{ '<var>trace_category</var>' }}", c"example_duration_raii");
    // Do some work
    duration!(c"{{ '<var>trace_category</var>' }}", c"nested duration", {{ '<var>argument_3</var>' }} => {{ '<var>argument_3_value</var>' }}, {{ '<var>argument_4</var>' }} => {{ '<var>argument_4_string</var>' }});
    // nested_duration closes due to RAII
    // trace_duration_raii closes due to RAII
  }
  ```

In Perfetto, a nested stack of durations looks as follows:

![A screenshot of the Perfetto trace viewer showing a timeline of events, with
the time from left to right. The top bar shows the text
"example_trace_provider.com 1617970". Below that, the timeline is split into two
sections, "initial-thread 1617972". Each of these has a bar showing the duration
of the event that is associated with each thread. Duration events with different
names are displayed with different colors: olive, green, blue, and dark green.
The duration events are stacked downwards, so the shorter durations are
contained within the larger duration above them.](images/duration_event_perfetto.png "A view of a duration event from a trace using the Perfetto Viewer"){: width="600"}

### Counter event {#counter-event}

A counter event captures numeric samples that are typically represented in
Perfetto as a stacked area chart. When you use a counter event, you can use the
`id` to distinguish between multiple instances of counters that share the same
category and name within the system process. You can associate 1-15 numeric
arguments with an event, each of which is interpreted as a distinct time series.

This example shows a `counter_id` value and a data series passed to a counter
event:

Note: Counter `id`s must be unique for a given process, category, and name
combination.

* {C }

  ```c
  trace_counter_id_t counter_id = 555;
  uint32_t data = get_some_data();
  TRACE_COUNTER("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", counter_id, "{{ '<var>data_series</var>' }}", data);
  ```

* {C++}

  ```cpp
  trace_counter_id_t counter_id = 555;
  uint32_t data = get_some_data();
  TRACE_COUNTER("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", counter_id, "{{ '<var>data_series</var>' }}", data);
  ```

* {Rust}

  ```rust
  let counter_id = 555;
  let data = get_some_data();
  counter!("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", counter_id, "{{ '<var>data_series</var>' }}" => data);
  ```

In Perfetto, a counter event looks as follows:

![A screenshot of the Perfetto trace viewer that shows the
`example_counter:somedataseries` counter event. To the right of the counter
event name there is a graph that rises over time, showing the counter increasing
in value.](images/counter_event_perfetto.png "A view of a counter event from a trace using the Perfetto Viewer"){: width="600"}

### Flow event {#flow-event}

A flow event describes flow handoffs between threads and across processes. These
events must be enclosed in a duration event which represents where the flow
handoff occurs. Each flow event may also attach arguments. A flow begin
(`TRACE_FLOW_BEGIN` or `flow_begin`) event may be followed by a flow step
(`TRACE_FLOW_STEP` or `flow_step`) event.

For example:

* {C }

  ```c
  trace_flow_id_t flow_id = 555;
  TRACE_FLOW_BEGIN("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_flow_name</var>' }}", flow_id, "{{ '<var>argument_1</var>' }}", {{ '<var>argument_1_value</var>' }});
  TRACE_FLOW_STEP("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_flow_step_name</var>' }}", flow_id);
  TRACE_FLOW_END("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_flow_name</var>' }}", flow_id);
  ```

* {C++}

  ```cpp
  trace_flow_id_t flow_id = 555;
  TRACE_FLOW_BEGIN("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_flow_name</var>' }}", flow_id, "{{ '<var>argument_1</var>' }}", {{ '<var>argument_1_value</var>' }});
  TRACE_FLOW_STEP("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_flow_step_name</var>' }}", flow_id);
  TRACE_FLOW_END("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_flow_name</var>' }}", flow_id);
  ```

* {Rust}

  ```rust
  let flow_id = 555;
  flow_begin!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_flow_name</var>' }}", flow_id, "{{ '<var>argument_1</var>' }}" => {{ '<var>argument_1_value</var>' }});
  flow_step!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_flow_step_name</var>' }}", flow_id);
  flow_end!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_flow_name</var>' }}", flow_id);
  ```

A flow event is represented as arrows in Perfetto, for example:

![A screenshot of the Perfetto trace viewer, showing a flow event of events on
different threads. Horizontal bars represent events, with their length
indicating duration. Each thread is connected with a curved arrow, which
indicates a sequence of events across these threads.](images/flow_event_perfetto.png "A view of a flow event from a trace using the Perfetto Viewer"){: width="600"}

### Async event {#async-event}

In a trace, instant and duration events are placed on the track of threads on
which they are produced. When working with multi-threaded async or thread pool
based code, events may be started and finished on different threads. Async
events allow specifying an `id` that links events across different threads
together and places these events on a single track.

An async begin (`TRACE_ASYNC_BEGIN` or `async_begin`) event may be followed by
async instant (`TRACE_ASYNC_INSTANT` or `async_instant`) events and must be
matched by an async end (`TRACE_ASYNC_END` or `async_end`) event with the same
category, name, and `id`.

Asynchronous events describe work which happens asynchronously and which may
span multiple threads. The `id` helps to correlate the progress of distinct
asynchronous operations which share the same category and name within the
same process. Asynchronous events within the same process that have matching
categories and ids are nested.

You can associate 0-15 arguments with an async event, each of which is used to
annotate the asynchronous operation with additional information. The arguments
that you provide to matching async begin, async instant, and async end events
are combined together in the trace; you do not need to repeat them.

* {C }

  ```c
  trace_async_id_t async_id = 555;
  TRACE_ASYNC_BEGIN("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_async_name</var>' }}", async_id, "{{ '<var>argument_1</var>' }}", {{ '<var>argument_1_value</var>' }});
  TRACE_ASYNC_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_async_instant_name</var>' }}", async_id);
  TRACE_ASYNC_END("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_async_name</var>' }}", async_id);
  ```

* {C++}

  ```cpp
  trace_async_id_t async_id = 555;
  TRACE_ASYNC_BEGIN("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_async_name</var>' }}", async_id, "{{ '<var>argument_1</var>' }}", {{ '<var>argument_1_value</var>' }});
  TRACE_ASYNC_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_async_instant_name</var>' }}", async_id);
  TRACE_ASYNC_END("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_async_name</var>' }}", async_id);
  ```

* {Rust}

  ```rust
  let async_id = 555;
  async_begin!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_async_name</var>' }}", async_id, "{{ '<var>argument_1</var>' }}" => {{ '<var>argument_1_value</var>' }});
  async_instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_async_instant_name</var>' }}", async_id);
  async_end!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_async_name</var>' }}", async_id);
  ```

In Perfetto, an async event is placed on its own named track and isn't placed
on the track of the thread that it was emitted from. An async event looks as
follows:

![A screenshot of the Perfetto trace viewer showing two async events:
`example_async` and `example_async2`. The `example_async` event displays a
`example_nested_async` below it. The screenshot also shows 2 blue arrows in the
`example_async` event and 2 blue arrows in the `example_async2` event which
indicate instant events.](images/async_event_perfetto.png "A view of a flow event from a trace using the Perfetto Viewer"){: width="600"}

## Arguments {#arguments}

You can use the following types of events in a Fuchsia trace:

* Null arguments
* Numeric types
* String types
* Pointers
* KOID
* Booleans

### Null arguments {#null-arguments}

A null argument is just a name a no data.

For example:

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "SomeNullArg", TA_NULL());
  ```

* {C++}

  ```cpp
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "SomeNullArg", nullptr);
  ```

* {Rust}

  ```rust
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, c"SomeNullArg" => ());
  ```

### Numeric types {#numeric-types}

A numeric type is any `int` or `float` data type.

For example:

* {C }

  ```c
  // Standard int types
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someuint32", TA_UINT32(2145));
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someuint64", TA_UINT64(423621626134123415));
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someint32", TA_INT32(-7));
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someint64", TA_INT64(-234516543631231));
  // Doubles
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Somedouble", TA_DOUBLE(3.1415));
  ```

* {C++}

  ```cpp
  // Standard int types
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someuint32", 2145);
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someuint64", 423621626134123415);
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someint32", -7);
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Someint64", -234516543631231);
  // Doubles
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "Somedouble", 3.1415);
  ```

* {Rust}

  ```rust
  // Standard int types
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, c"Someuint32" => 2145);
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, c"Someuint64" => 423621626134123415);
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, c"Someint32" => -7);
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, c"Someint64" => -234516543631231);
  // Doubles
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, c"Somedouble" => 3.1415);
  ```

### String types {#string-types}

A string type is any `string` data type.

For example:

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "ping", "pong");
  ```

* {C++}

  ```cpp
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "ping", "pong");
  ```

* {Rust}

  ```rust
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, "ping" => "pong");
  ```

### Pointers {#pointers}

A pointer is formatted in hex in the Perfetto viewer. You can also specifically
specify the trace argument type when type deduction would otherwise resolve
to the wrong data type.

For example:

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "somepointer", &i, "someotherpointer", &i, TA_POINTER(0xABCD));
  ```

* {C++}

  ```cpp
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "somepointer", &i, "someotherpointer", &i, 0xABCD);
  ```

* {Rust}

  ```rust
  let x: u64 = 123;
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, "SomeOtherPointer" => &x as *const u64);
  ```

### KOID {#koid}

A KOID is the id of a kernel object and have their own data type to distinguish
them from regular `u64` data types.

For example:

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "somekoid", TA_KOID(0x0012));
  ```

* {C++}

  ```cpp
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "somekoid", TA_KOID(0x0012));
  ```

* {Rust}

  ```rust
  let koid: zx::Koid = vmo.get_koid()?;
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, "somekoid" => koid);
  ```

### Booleans {#booleans}

A boolean displays as either `true` or `false`.

For example:

* {C }

  ```c
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "somebool", TA_BOOL(true));
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "someotherbool", TA_BOOL(false));
  ```

* {C++}

  ```cpp
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "somebool", true);
  TRACE_INSTANT("{{ '<var>trace_category</var>' }}", "{{ '<var>trace_name</var>' }}", TRACE_SCOPE_THREAD, "someotherbool", false);
  ```

* {Rust}

  ```rust
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, "somebool" => true);
  instant!(c"{{ '<var>trace_category</var>' }}", c"{{ '<var>trace_name</var>' }}", Scope::Thread, "someotherbool" => false);
  ```

[trace-format-event-record]: /docs/reference/tracing/trace-format.md#event-record
[rustdoc-fuchsia_trace]: https://fuchsia-docs.firebaseapp.com/rust/fuchsia_trace/index.html
[fuchsia-tracing-c-macros]: /docs/reference/tracing/c_cpp_macros.md
[trace-provider-cpp-example]: /src/performance/example_trace_provider/README.md