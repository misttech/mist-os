# Trace Processor Library

This directory contains a Python library which can deserialize and extract metrics from JSON-based traces in the FXT format.

Library modules include:
1. `trace_importing` - Code to deserialize the Fuchsia trace JSON into an in-memory data model.
1. `trace_model` - The definition of the in-memory trace data model.
1. `trace_time` - Nanosecond-resolution time types for use by the trace data model.
1. `trace_utils` - Utilities to extract and filter trace events from a data model.

The library is based on the original version of this code located at
`sdk/testing/sl4f/client/lib/src/trace_processing/` and `sdk/testing/sl4f/client/test/`

Binary module includes:
1. `run_cpu_breakdown` - Standalone processing on a Fuchsia trace JSON that outputs information about CPU metrics.

How to use `run_cpu_breakdown`:
1. `fx set` with the flag: `--with-host //src/performance/lib/trace_processing:run_cpu_breakdown`
1. `fx build`
1. `fx run_cpu_breakdown <path to trace JSON> <path to output>`

How to run tests for `cpu_breakdown`:
1. `fx set` with the flag: `--with-host //src/performance/lib:tests,//src/performance/lib/trace_processing:run_cpu_breakdown`
1. `fx build`
1. `fx test --host`
