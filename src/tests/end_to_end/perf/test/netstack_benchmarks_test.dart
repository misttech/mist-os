// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/84961): Fix null safety and remove this language version.
// @dart=2.9

import 'dart:io';

import 'package:test/test.dart';

import 'helpers.dart';

// Runs a test where the purpose is not to produce performance metrics as
// output, but just to produce a trace for manual inspection.
//
// When the test is run on Infra, the trace is made available for download via
// the "task outputs" link. Currently this host-side wrapper code is needed for
// making the trace available that way, but in the future that could be done
// by using "ffx test" to run the test on Infra (see https://fxbug.dev/125224).
Future<void> runSocketBenchmarksWithTracing(String componentName) async {
  final helper = await PerfTestHelper.make();

  final timestamp = DateTime.now().microsecondsSinceEpoch;
  final String targetOutputDir = '/tmp/perftest_$timestamp';
  try {
    final String command = 'run-test-suite'
        ' fuchsia-pkg://fuchsia.com/socket-benchmarks-with-tracing-pkg#meta/$componentName.cm'
        ' --deprecated-output-directory $targetOutputDir';
    final result = await helper.sl4fDriver.ssh.run(command);
    expect(result.exitCode, equals(0));

    // Search for the output file within the directory structure
    // produced by run-test-suite.
    final findResult = await helper.sl4fDriver.ssh
        .run('find $targetOutputDir -name trace.fxt');
    final String findOutput = findResult.stdout.trim();
    expect(findOutput, isNot(anyOf(equals(''), contains('\n'))));
    final File traceFile = await helper.storage
        .dumpFile(findOutput, '$componentName-trace', 'fxt');
  } finally {
    // Clean up: remove the output tree.
    final result = await helper.sl4fDriver.ssh.run('rm -r $targetOutputDir');
    expect(result.exitCode, equals(0));
  }
}

void main() {
  enableLoggingOutput();

  test('socket_benchmarks', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.loopback.txt');
  }, timeout: Timeout.none);

  test('socket_benchmarks_with_fast_udp', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks-with-fast-udp.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.loopback.fastudp.txt');
  }, timeout: Timeout.none);

  test('socket_benchmarks_with_netstack3', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks-with-netstack3.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile:
            'fuchsia.network.socket.loopback.netstack3.txt');
  }, timeout: Timeout.none);

  test('socket_benchmarks_with_fake_netstack', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks-with-fake-netstack.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile:
            'fuchsia.network.socket.loopback.fake_netstack.txt');
  }, timeout: Timeout.none);

  test('udp_serde_benchmarks', () async {
    await runTestComponent(
        packageName: 'udp-serde-benchmarks',
        componentName: 'udp-serde-benchmarks.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.udp_serde.txt');
  }, timeout: Timeout.none);

  // TODO(https://fxbug.dev/125224): Remove this when the target-side only test
  // is sufficient for generating the trace as an artifact.
  test('socket_benchmarks_with_tracing',
      () => runSocketBenchmarksWithTracing('socket-benchmarks-with-tracing'),
      timeout: Timeout.none);

  // TODO(https://fxbug.dev/125224): Remove this when the target-side only test
  // is sufficient for generating the trace as an artifact.
  test(
      'socket_benchmarks_with_fast_udp_tracing',
      () => runSocketBenchmarksWithTracing(
          'socket-benchmarks-with-fast-udp-tracing'),
      timeout: Timeout.none);

  test('resource_usage_benchmarks', () async {
    await runTestComponent(
        packageName: 'resource-usage-benchmarks',
        componentName: 'resource-usage-benchmark-netstack2.cm',
        expectedMetricNamesFile: 'fuchsia.netstack.resource_usage.txt');
  }, timeout: Timeout.none);

  test('resource_usage_benchmarks_with_netstack3', () async {
    await runTestComponent(
        packageName: 'resource-usage-benchmarks',
        componentName: 'resource-usage-benchmark-netstack3.cm',
        expectedMetricNamesFile:
            'fuchsia.netstack.resource_usage.netstack3.txt');
  }, timeout: Timeout.none);
}
