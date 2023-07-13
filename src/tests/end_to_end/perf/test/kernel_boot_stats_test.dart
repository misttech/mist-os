// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('fuchsia.kernel.boot', () async {
    await runTestComponent(
        packageName: 'kernel-boot-benchmarks',
        componentName: 'kernel-boot-benchmarks.cm',
        realm: '/core/testing:system-tests',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.kernel.boot.txt');
  }, timeout: Timeout.none);
}
