# Memory allocation and deallocation trace

This experimental tool intercepts allocations and deallocation and logs the
events to the fuchsia tracing system with the category `memory_trace`.

## Prerequisite

-   Add the following option to fx set: `--with-host
    //src/performance/memory/profile:fxt_to_pprof --with-base //bundles/tools`
-   Ensure that `pprof` and `gzip` binaries are in the environment path.

## Getting started

-   Dynamically link the component with `libmemory_trace.so`
    (`//src/performance/memory/profile:memory_trace`).
-   [Rust only] Add `configs = [ "//src/performance/memory/profile:no-as-needed"
    ]` to the `rustc_binary`.
-   Route `fuchsia.tracing.provider.Registry` to the component.
-   Ensure your component
    [registers a trace provider](https://fuchsia.dev/fuchsia-src/development/tracing/tutorial/registering-a-trace-provider?hl=en#register-with-the-trace-manager).

The dynamic library registers hooks the Fuchsia default allocator for c and rust
programs.

The memory profile collection can be started and stopped with `ffx` tool,
however an helper script is provided to automate the trace post processing.

Know that trace collection modes commes with limitations: - streaming can drop
samples when the tracing rate is high. - oneshot stops collecting without user
feedback in ffx when the buffer is full. - circular is not usable because the
first records of the memory profile are lost. Unfortunately they contains data
required for symbolization.

`bash src/performance/memory/profile/profile_memory.sh`

This tool does the following:

1.  Starts the trace collection: `ffx trace start --buffering-mode circular
    --categories memory_profile`
2.  Converts the `fxt` trace to `pprof` protocol buffer format with
    `//src/performance/memory/profile:memory_trace`
3.  Gzip the resulting protobuf
4.  Convert the build-id directory to a format supported by `pprof`
5.  Starts `pprof` and upload the profile to a server

## Try it

-   Add the following option to fx set: `--with
    //src/performance/memory/profile/example`
-   Start collection: `bash src/performance/memory/profile/profile_memory.sh`
-   Start the binary: `ffx component create
    /core/session-manager/session:hello-memory-profiler
    fuchsia-pkg://fuchsia.com/hello-memory-profiler#meta/hello-memory-profiler.cm`
    `ffx component start /core/session-manager/session:hello-memory-profiler`

## Report

The profile contains the following values:

* **new object** is the number of new allocations for the duration of the pprof
  sampling session.
* **new allocated** is the number of bytes contained in those new allocations.
* **residual object** is the number of objects that were allocated during the
  session, and remain allocated at the end of the session.
* **residual bytes** is the number of bytes in residual objects.

## Development

Run the test with fx set: `--with //src/performance/memory/profile:tests`

Then: `fx test //src/performance/memory/profile`
