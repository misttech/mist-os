// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'dart:io' show File;

import 'package:test/test.dart';

import 'helpers.dart';

List<void Function()> _tests = [];

const int perftestProcessRuns = 6;

String tmpPerfResultsJson(String benchmarkBinary) {
  return '/tmp/perf_results_$benchmarkBinary.json';
}

// Runs a benchmark that uses the C++ perftest runner.
// It is believed that benchmarks converge to different means in different
// process runs (and reboots). Since each of these benchmarks are currently
// fast to run (a few secs), run the binary several times for more stability.
void runPerftestFidlBenchmark(
    String benchmarkBinary, String expectedMetricNamesFile) {
  final resultsFile = tmpPerfResultsJson(benchmarkBinary);
  _tests.add(() {
    test(benchmarkBinary, () async {
      final helper = await PerfTestHelper.make();

      final List<File> resultsFiles = [];
      for (var process = 0; process < perftestProcessRuns; ++process) {
        final result = await helper.sl4fDriver.ssh
            .run('/bin/$benchmarkBinary -p --quiet --out $resultsFile');
        expect(result.exitCode, equals(0));
        resultsFiles.add((await helper.storage.dumpFile(
            resultsFile,
            'results_fidl_microbenchmarks_process$process',
            'fuchsiaperf_full.json'))!);
      }
      await helper.processResultsSummarized(resultsFiles,
          expectedMetricNamesFile: expectedMetricNamesFile);
    }, timeout: Timeout.none);
  });
}

void main(List<String> args) {
  enableLoggingOutput();

  runPerftestFidlBenchmark(
      'lib_fidl_microbenchmarks', 'fuchsia.fidl_microbenchmarks.libfidl.txt');
  runPerftestFidlBenchmark('reference_fidl_microbenchmarks',
      'fuchsia.fidl_microbenchmarks.reference.txt');

  _tests
    ..add(() {
      test('go_fidl_microbenchmarks', () async {
        final helper = await PerfTestHelper.make();
        await helper.runTestCommand(
            (resultsFile) =>
                '/bin/go_fidl_microbenchmarks --out_file $resultsFile',
            expectedMetricNamesFile: 'fuchsia.fidl_microbenchmarks.go.txt');
      }, timeout: Timeout.none);
    });

  runShardTests(args, _tests);
}
