// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('diagnostics_log_rust_encoding_benchmarks', () async {
    await runTestComponent(
        packageName: 'diagnostics-log-rust-benchmarks',
        componentName: 'encoding.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.diagnostics_log_rust.encoding.txt');
  }, timeout: Timeout.none);

  test('diagnostics_log_rust_core_benchmarks', () async {
    await runTestComponent(
        packageName: 'diagnostics-log-rust-benchmarks',
        componentName: 'core.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        expectedMetricNamesFile: 'fuchsia.diagnostics_log_rust.core.txt');
  }, timeout: Timeout.none);
}
