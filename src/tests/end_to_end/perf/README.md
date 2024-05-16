# Performance tests

This directory contains performance tests.

Check the [instructions][instructions] to run these test locally.

This directory contains some performance tests written in Python using
[Lacewing][lacewing]. It also contains all the expected metrics that performance
tests may generate. Finally, it includes a GN group that contains all benchmarks
in the tree.

This directory contains the following performance tests:

*   `flatland_benchmark` - Tests the performance of Flatland's
    end-to-end present latency, measured by tracing flow events, for minimal
    Flatland client (`flatland-view-provider`).

*   `input_latency_benchmark` - Tests the performance of
    end-to-end input latency, measured by tracing flow events, for minimal
    Scenic clients (`simplest-app-flatland-session`).

*  `tracing_microbenchmarks_test` - Tests the performance of the tracing
    subsystem.

The following examples:

*   `perf_publish_example` - Simple example test that publishes a performance
    metric.

And the following tests:

*   `perftest_trace_events_test` - Tests that we correctly read events from a
    trace session.

You can view the test results from CI builds in [Chromeperf][chromeperf].

<!-- Reference links -->

[chromeperf]: /docs/development/performance/chromeperf_user_guide.md
[instructions]: /docs/development/performance/running_performance_tests.md
[lacewing]: /src/testing/end_to_end/README.md
