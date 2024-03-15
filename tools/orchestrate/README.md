# Shared Test Orchestration Tool

This directory contains Fuchsia's shared test orchestrator and fixtures.

Design: http://go/shared-infra-test-orchestration

The `orchestrate` tool (located at `//tools/orchestrate/cmd:orchestrate`) runs
as the swarming test bot entrypoint for all OOT Bazel-based repositories and
for google3 ftx tests.

## Distribution
The tool is uploaded to CIPD using the host_prebuilts-* CI builders and rolled
to fuchsia-infra-bazel-rules for use in OOT Bazel-based repositories.

It is also included in the vendor/google IDK for distribution to google3 to
allow for testing via fargon.
See http://go/orchestrate-cipd-distribution.

## Self Testing
Orchestrate has go unittests that can be run, but it's typically more useful to
run e2e tests by adding the following footer in your commit message: \
`Cq-Include-Trybots: luci.fuchsia.try:sdk-bazel-linux-fuchsia_infra_bazel_rules`

Alternatively, you can also select manually select the
`sdk-bazel-linux-fuchsia_infra_bazel_rules` tryjob on any cl.

## Shared Fixtures
TODO(b/315216126): Make a note about test fixtures here.
