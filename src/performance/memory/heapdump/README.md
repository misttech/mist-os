# Heapdump memory profiler

Memory profiling tool that can capture snapshots of live allocations at any
point in time and export them in a pprof-compatible protobuf format.

**Note**: Development of this tool is still work in progress.

## How to use

* Pass `--with src/performance/memory/heapdump/collector` to the `fx set`
  invocation.
* Add `//src/performance/memory/heapdump/instrumentation` to the `deps` of the
  `executable` target that you want to profile.
* Route the `fuchsia.memory.heapdump.process.Registry` capability from collector
  to your component.
* Add `#include <heapdump/bind_with_fdio.h>` and call
  `heapdump_bind_with_fdio()` at the beginning of `main` in your program.
* Run your program as usual.
* Use `ffx profile heapdump snapshot` while your program is running to take a
  snapshot of all the current live allocations. For instance, assuming that your
  program is called `example.cm`:

```
ffx profile heapdump snapshot --by-name example.cm --output-file my_snapshot.pb
```

* Use [pprof](https://github.com/google/pprof) to analyze the memory profile you
  acquired:

```
export PPROF_BINARY_PATH=~/fuchsia/out/default/.build-id:~/fuchsia/prebuilt/third_party/clang/linux-x64/lib/debug/.build-id:~/fuchsia/prebuilt/third_party/rust/linux-x64/lib/debug/.build-id
pprof -http=":" my_snapshot.pb
```

## Design

The instrumentation library intercepts all allocation and deallocation events,
and it keeps track of all live allocations by storing them into a specific VMO
(called "allocations VMO"), which is organized as an hash table containing
the allocated addresses as the keys and metadata as the values.

Each instrumented process shares a read-only handle to its VMOs to a centralized
component called "heapdump-collector". The collector can then easily take a
snapshot, at any time and without any further cooperation from the instrumented
process, by simply creating a `ZX_VMO_CHILD_SNAPSHOT` of the allocations VMO.

In order to guarantee that the resulting snapshot is always consistent, the
instrumentation updates the hash table atomically (i.e. inserting/removing an
allocation corresponds to single atomic operation).

### VMO format

The instrumentation library writes into the shared VMOs and the collector must
be able to correctly parse this data. It is therefore important that they agree
on the data structures' layout.

Because of the atomicity requirement, it is not possible to simply use FIDL to
serialize data into the VMO. Instead, heapdump's `heapdump_vmo` crate contains
ad hoc functions to manipulate the VMOs, that are used by both the
instrumentation and the collector.

However, while the usage of the shared `heapdump_vmo` crate makes it easy to
agree on a common format, it does not solve the issue of forward-compatibility:
we want to support an older instrumentation library connecting to a newer
collector, while retaining the possibility to change the VMO format in a
breaking way. This is the reason why all data structures defined in
`heapdump_vmo` have a version suffix (e.g. `_v1`): the current instrumentation
library always uses the latest version to operate its VMOs but, by making it
possible for different data structure versions to coexist in the code base, the
collector can keep supporting processes linked against older instrumentation
libraries.

The instrumentation library implicitly communicates the version of its data
structures when it registers to the collector over the
`fuchsia.memory.heapdump.process.Registry` FIDL protocol. In particular:

* calling `RegisterV1` corresponds to `allocations_table_v1` and
  `resources_table_v1`

**Note**: only one version has been defined at the moment.
