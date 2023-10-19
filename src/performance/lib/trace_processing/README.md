# Trace Processor Library

This directory contains a Python library which can deserialize and extract metrics from JSON-based traces in the FXT format.

Library modules include:
1. `trace_importing` - Code to deserialize the FXT JSON into an in-memory data model.
1. `trace_model` - The definition of the in-memory trace data model.
1. `trace_time` - Nanosecond-resolution time types for use by the trace data model.
1. `trace_utils` - Utilities to extract and filter trace events from a data model.

The library is based on the original version of this code located at
`sdk/testing/sl4f/client/lib/src/trace_processing/` and `sdk/testing/sl4f/client/test/`
