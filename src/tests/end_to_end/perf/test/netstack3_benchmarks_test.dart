// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('netstack3_benchmarks', () async {
    await runTestComponent(
        packageName: 'netstack3_benchmarks',
        componentName: 'netstack3_benchmarks.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.netstack3.core.txt');
  }, timeout: Timeout.none);
}
