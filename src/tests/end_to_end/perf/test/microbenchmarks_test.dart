// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  // We run the fuchsia_microbenchmarks process multiple times.  That is
  // useful for tests that exhibit between-process variation in results
  // (e.g. due to memory layout chosen when a process starts) -- it
  // reduces the variation in the average that we report.
  const int processRuns = 6;

  // We override the default number of within-process iterations of
  // each test case and use a lower value.  This reduces the overall
  // time taken and reduces the chance that these invocations hit
  // Infra Swarming tasks' IO timeout (swarming_io_timeout_secs --
  // the amount of time that a task is allowed to run without
  // producing log output).
  const int iterationsPerTestPerProcess = 120;

  test('fuchsia_microbenchmarks', () async {
    await runTestComponent(
        packageName: 'fuchsia_microbenchmarks',
        componentName: 'fuchsia_microbenchmarks.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}'
            ' --runs $iterationsPerTestPerProcess',
        processRuns: processRuns,
        expectedMetricNamesFile: 'fuchsia.microbenchmarks.txt');
  }, timeout: Timeout.none);
}
