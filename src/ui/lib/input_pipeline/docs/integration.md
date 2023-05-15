# input_pipeline > Integration

Reviewed on: 2022-03-22

This document describes how to integrate the input pipeline within a
larger Rust program. For example, the input pipeline is integrated with [Scene Manager](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/ui/bin/scene_manager/) for some builds.

## Requirements

The code that deals with the input pipeline must run on a `LocalExecutor`. For
existing code, this is already true. Both `//src/ui/bin/input-pipeline` and
`//src/session/bin/scene_manager` use `fuchsia_async::run_singlethreaded`,
so they each use a `LocalExecutor`.

## Alternatives

A program might want to isolate itself from the risk that a bug in the input
pipeline library (e.g. infinite loop) will keep the rest of the program from
making progress. Such programs can

1. Spawn a dedicated thread to run a `LocalExecutor` which creates and runs
   the input pipeline, OR
1. Run the input pipeline as a standalone component, and communicate with
   the pipeline over FIDL.

## Component Manifest

The library provides a [client shard](../meta/client.shard.cml) to ensure
proper routing is available. This shard can be included in the integrating
component's manifest file.
