// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/125224): Remove all the benchmarks in this file when
// the target-side only test is sufficient for generating the trace as an
// artifact.

// @dart=2.12

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
Future<void> runBenchmarksWithTracing(
    String packageName, String componentName) async {
  final helper = await PerfTestHelper.make();

  final timestamp = DateTime.now().microsecondsSinceEpoch;
  final String targetOutputDir = '/tmp/perftest_$timestamp';
  try {
    final String command = 'run-test-suite'
        ' fuchsia-pkg://fuchsia.com/$packageName#meta/$componentName.cm'
        ' --realm /core/testing:system-tests'
        ' --deprecated-output-directory $targetOutputDir';
    final result = await helper.sl4fDriver.ssh.run(command);
    expect(result.exitCode, equals(0));

    // Search for the output file within the directory structure
    // produced by run-test-suite.
    final findResult = await helper.sl4fDriver.ssh
        .run('find $targetOutputDir -name trace.fxt');
    final String findOutput = findResult.stdout.trim();
    expect(findOutput, isNot(anyOf(equals(''), contains('\n'))));
    final File? traceFile = await helper.storage
        .dumpFile(findOutput, '$packageName-$componentName-trace', 'fxt');
  } finally {
    // Clean up: remove the output tree.
    final result = await helper.sl4fDriver.ssh.run('rm -r $targetOutputDir');
    expect(result.exitCode, equals(0));
  }
}

void main() {
  enableLoggingOutput();

  test(
      'socket_benchmarks_with_tracing',
      () => runBenchmarksWithTracing('socket-benchmarks-with-tracing-pkg',
          'socket-benchmarks-with-tracing'),
      timeout: Timeout.none);

  test(
      'socket_benchmarks_with_fast_udp_tracing',
      () => runBenchmarksWithTracing('socket-benchmarks-with-tracing-pkg',
          'socket-benchmarks-with-fast-udp-tracing'),
      timeout: Timeout.none);

  test(
      'socket_benchmarks_with_netstack3_tracing',
      () => runBenchmarksWithTracing('socket-benchmarks-with-tracing-pkg',
          'socket-benchmarks-with-netstack3-tracing'),
      timeout: Timeout.none);

  test(
      'tun_socket_benchmarks_with_tracing',
      () => runBenchmarksWithTracing(
          'tun-socket-benchmarks', 'tun-socket-benchmarks-ns2-with-tracing'),
      timeout: Timeout.none);

  test(
      'tun_socket_benchmarks_with_netstack3_tracing',
      () => runBenchmarksWithTracing(
          'tun-socket-benchmarks', 'tun-socket-benchmarks-ns3-with-tracing'),
      timeout: Timeout.none);
}
