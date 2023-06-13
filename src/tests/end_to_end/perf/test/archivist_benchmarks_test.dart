// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('archivist_logging_benchmarks', () async {
    await runTestComponent(
        packageName: 'archivist-benchmarks',
        componentName: 'logging.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.archivist.logging.txt');
  }, timeout: Timeout.none);

  test('archivist_formatter_benchmarks', () async {
    await runTestComponent(
        packageName: 'archivist-benchmarks',
        componentName: 'formatter.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.archivist.formatter.txt');
  }, timeout: Timeout.none);
}
