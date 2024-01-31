# Shared Test Orchestration Tool

This directory contains Fuchsia's shared test orchestrator and fixtures.

Design: http://go/shared-infra-test-orchestration

The `orchestrate` tool (located at `//tools/orchestrate/cmd:orchestrate`) runs
as the swarming test bot entrypoint for all OOT Bazel-based repositories and
for google3 ftx tests.

## Distribution
The tool's source (`//tools/orchestrate`) is copied from fuchsia.git to google3
as well as the fuchsia-infra-bazel-rules repository via copybara and built in
those repositories.

**Note: Because of this please take special care whenever adding dependencies on
targets outside of `//tools/orchestrate`.**

## Self Testing
Orchestrate has go unittests that can be run, but it's typically more useful to
run e2e tests by selecting the `orchestrate-e2e` tryjob on any cl.

## Shared Fixtures
TODO(b/315216126): Make a note about test fixtures here.
