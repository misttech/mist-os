// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'dart:io' show Platform;

import 'package:test/test.dart';

import 'helpers.dart';

const _catapultConverterPath = 'runtime_deps/catapult_converter';

Future<void> _runStorageStarnixBenchmarks({
  required String componentName,
  required String expectedMetricNamesFile,
}) async {
  final helper = await PerfTestHelper.make();
  final resultsFileFull = await helper.runTestComponentReturningResultsFile(
      packageName: 'storage-starnix-benchmarks',
      componentName: componentName,
      realm: '/core/testing:starnix-tests',
      commandArgs: '',
      resultsFileSuffix: '');

  // Using the fuchsiaperf_full file like this avoids the processing done by
  // summarize.dart. This is for two reasons:
  // 1) To keep the initial iterations' times instead of dropping them
  //    (see src/storage/benchmarks/README.md).
  // 2) To allow standard deviations to be reported to Chromeperf so that
  //    Chromeperf displays them in its graphs.
  const fuchsiaPerfFullSuffix = 'fuchsiaperf_full.json';
  expect(resultsFileFull.path, endsWith(fuchsiaPerfFullSuffix));
  final resultsFile = await resultsFileFull.rename(resultsFileFull.path
      .replaceRange(resultsFileFull.path.length - fuchsiaPerfFullSuffix.length,
          null, 'fuchsiaperf.json'));

  await helper.performance.convertResults(
      _catapultConverterPath, resultsFile, Platform.environment,
      expectedMetricNamesFile: expectedMetricNamesFile);
}

void main() {
  enableLoggingOutput();

  test('storage-starnix-data-benchmarks', () async {
    await _runStorageStarnixBenchmarks(
      componentName: 'storage-starnix-data-benchmarks.cm',
      expectedMetricNamesFile: 'fuchsia.storage.starnix_data.txt',
    );
  }, timeout: Timeout.none);

  test('storage-starnix-tmp-benchmarks', () async {
    await _runStorageStarnixBenchmarks(
      componentName: 'storage-starnix-tmp-benchmarks.cm',
      expectedMetricNamesFile: 'fuchsia.storage.starnix_tmp.txt',
    );
  }, timeout: Timeout.none);
}
