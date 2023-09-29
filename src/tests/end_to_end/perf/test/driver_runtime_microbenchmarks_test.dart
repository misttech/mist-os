// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('/bin/driver_runtime_microbenchmarks', () async {
    await runTestComponent(
        packageName: 'driver_runtime_microbenchmarks',
        componentName: 'driver_runtime_microbenchmarks.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.driver_runtime_microbenchmarks.txt');
  }, timeout: Timeout.none);
}
