# Add tracing in your code

This page describes how to add tracing to your Fuchsia component's code.

## Prerequisites

Before you begin, make sure you have completed the following tasks:

* [Familiarize yourself with the Fuchsia tracing system][fuchsia-tracing-system].
* [Register your component as a tracing provider][register-a-trace-provider].
* [Include the `libtrace` library to capture trace data][libtrace-trace-event].

## Use tracing macros in your code {:#use-tracing-macros-in-your-code}

Once your component is registered as a [trace provider][register-a-trace-provider],
you can add tracing in your component's code.

The following actions are often useful and can easily be added in the code
using the tracing macros:

* [Trace an instant event](#trace-an-instant-event).
* [Disable tracing](#disable-tracing).
* [Determine if tracing is on](#determine-if-tracing-is-on).
* [Time an event](#time-an-event).

For the list of all available tracing macros, see
[Tracing: C and C++ macros][c-cpp-macros].

### Trace an instant event {:#trace-an-instant-event}

The following example (for C and C++) writes an instant event
representing a single moment in time:

```c
TRACE_INSTANT("helloworld", "hello_world_test", TRACE_SCOPE_PROCESS, "message", TA_STRING("Hello, World!"));
```

This example specifies a category of `helloworld`, a name of `hello_world_test`,
a scope of `TRACE_SCOPE_PROCESS`, and a key and value pair.

For more information on the `TRACE_INSTANT` macro, see
[`TRACE_INSTANT`][trace-instant].

### Disable tracing {:#disable-tracing}

There are cases where you may wish to entirely disable tracing (for
instance, when you are about to release the component into production).
If the `NTRACE` macro is added in your code, the tracing macros do not
generate any code.

The following example (for C and C++) shows the `NTRACE` macro:

```c
#define NTRACE  // disable tracing
#include <lib/trace/event.h>
```

Make sure that you define the `NTRACE` macro before the `#include`statement.

In the example below, the `rx_count` and `tx_count` fields are used only with
tracing, so if `NTRACE` is asserted, which indicates that tracing is disabled,
the fields do not take up space in the `my_statistics_t` structure.

```c
typedef struct {
#ifndef NTRACE  // reads as "if tracing is not disabled"
    uint64_t    rx_count;
    uint64_t    tx_count;
#endif
    uint64_t    npackets;
} my_statistics_t;
```

However, if you do need to conditionally compile the code for managing
the recording of the statistics, you can use the `TRACE_INSTANT` macro:

```c
#ifndef NTRACE
    status.tx_count++;
    TRACE_INSTANT("bandwidth", "txpackets", TRACE_SCOPE_PROCESS,
                  "count", TA_UINT64(status.tx_count));
#endif  // NTRACE
```

For more information on the `NTRACE` macro, see [`NTRACE`][ntrace].

### Determine if tracing is on {:#determine-if-tracing-is-on}

In some cases, you may need to determine if tracing is on at runtime.
If tracing is compiled in your code because `NTRACE` is not defined,
the `TRACE_ENABLED()` macro determines if tracing for your trace
provider is on. If tracing is compiled out, `TRACE_ENABLED()` always
returns false.

```c
#ifndef NTRACE
    if (TRACE_ENABLED()) {
        int v = do_something_expensive();
        TRACE_INSTANT(...
    }
#endif  // NTRACE
```

The example above (for C and C++) uses both the `#ifndef` and the
`TRACE_ENABLED()` macro together because the function
`do_something_expensive()` may not exist in the trace-disabled version
of your code.

For more information on the `TRACE_ENABLED` macro, see
[`TRACE_ENABLED`][trace-enabled].

### Time an event {:#time-an-event transformation="converted"}

If you need to time a function or procedure, see the example below (for C++)
from a [`blobfs`][blobfs-cc] vnode constructor:

```cpp
zx_status_t VnodeBlob::InitCompressed() {
    TRACE_DURATION("blobfs", "Blobfs::InitCompressed", "size", inode_.blob_size,
                   "blocks", inode_.num_blocks);
    ...
```

This example records the length of time spent in the constructor,
along with the size and number of blocks. Since this is a C++ example,
the data types can be inferred by the compiler.

For more information on the `TRACE_DURATION` macro, see
[`TRACE_DURATION`][trace-duration].

Once you have added tracing code to your component, you can now collect a
trace from the component. For more information, see the next
[Record and visualize a trace][record-and-visualize-a-trace] page.

<!-- Reference links -->

[fuchsia-tracing-system]: /docs/concepts/kernel/tracing-system.md
[register-a-trace-provider]: /docs/development/tracing/tutorial/register-a-trace-provider.md
[libtrace-trace-event]: /docs/reference/tracing/libraries.md#libtrace-trace-event
[c-cpp-macros]: /docs/reference/tracing/c_cpp_macros.md
[trace-instant]: /docs/reference/tracing/c_cpp_macros.md#TRACE_INSTANT
[ntrace]: /docs/reference/tracing/c_cpp_macros.md#NTRACE
[trace-enabled]: /docs/reference/tracing/c_cpp_macros.md#TRACE_ENABLED
[blobfs-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:/src/storage/blobfs/blobfs.cc
[trace-duration]: /docs/reference/tracing/c_cpp_macros.md#TRACE_DURATION
[record-and-visualize-a-trace]: /docs/development/tracing/tutorial/record-and-visualize-a-trace.md
