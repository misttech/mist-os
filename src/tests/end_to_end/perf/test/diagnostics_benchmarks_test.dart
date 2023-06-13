// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('selectors_benchmarks', () async {
    await runTestComponent(
        packageName: 'selectors-benchmarks',
        componentName: 'selectors-benchmarks.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.diagnostics.txt');
  }, timeout: Timeout.none);
}
