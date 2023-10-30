// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'dart:io' show File;
import 'dart:convert';

import 'package:sl4f/trace_processing.dart';
import 'package:test/test.dart';

import 'helpers.dart';

const _trace2jsonPath = 'runtime_deps/trace2json';

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

  // For running with tracing enabled we run the following tests:
  //
  // - The Tracing suite, which creates exercises the trace-engine code via
  //   TRACE macros
  // - Syscall/Null, Syscall/ManyArgs, and Channel/WriteRead which exercise
  //   writing events to the kernel trace buffer
  const filterRegex =
      "'(^Tracing/)|(^Syscall/Null\$)|(^Syscall/ManyArgs\$)|(^Channel/WriteRead/1024bytes/1handles\$)'";

  // Modify the given fuchsiaperf.json file to add a suffix to all of
  // the test_suite fields, to allow distinguishing between test
  // variants.
  void addTestSuiteSuffix(File resultsFile, String suffix) {
    final jsonData = jsonDecode(resultsFile.readAsStringSync());
    for (final testResult in jsonData) {
      testResult['test_suite'] += suffix;
    }
    resultsFile.writeAsStringSync(jsonEncode(jsonData));
  }

  // Run some of the microbenchmarks with tracing enabled to measure the
  // overhead of tracing.
  test('fuchsia_microbenchmarks_tracing_categories_enabled', () async {
    final helper = await PerfTestHelper.make();

    final List<File> resultsFiles = [];
    for (var process = 0; process < processRuns; ++process) {
      final traceSession = await helper.performance.initializeTracing(
          categories: ['kernel', 'benchmark'], bufferSize: 36);
      await traceSession.start();

      final resultsFile = await helper.runTestComponentReturningResultsFile(
          packageName: 'fuchsia_microbenchmarks',
          componentName: 'fuchsia_microbenchmarks.cm',
          commandArgs: '-p --quiet'
              ' --out ${PerfTestHelper.componentOutputPath}'
              ' --runs $iterationsPerTestPerProcess'
              ' --filter $filterRegex --enable-tracing',
          resultsFileSuffix: '_process$process');
      addTestSuiteSuffix(resultsFile, '.tracing');
      resultsFiles.add(resultsFile);

      await traceSession.stop();

      const testName = 'fuchsia_microbenchmarks_tracing_categories_enabled';
      final fxtTraceFile = await traceSession.terminateAndDownload(testName);
      final jsonTraceFile = await helper.performance
          .convertTraceFileToJson(_trace2jsonPath, fxtTraceFile);

      // Check that the trace contains the expected trace events.
      final Model model = await createModelFromFile(jsonTraceFile);

      var events = filterEvents(getAllEvents(model), category: 'benchmark');
      for (final eventName in [
        'InstantEvent',
        'ScopedDuration',
        'DurationBegin'
      ]) {
        expect(events.where((event) => event.name == eventName).length,
            iterationsPerTestPerProcess,
            reason: 'Mismatch for $eventName');
      }

      events = filterEvents(getAllEvents(model), category: 'kernel:syscall');
      for (final eventName in ['syscall_test_0', 'syscall_test_8']) {
        expect(events.where((event) => event.name == eventName).length,
            iterationsPerTestPerProcess,
            reason: 'Mismatch for $eventName');
      }
    }
    await helper.processResultsSummarized(resultsFiles,
        expectedMetricNamesFile: 'fuchsia.microbenchmarks.tracing.txt');
  }, timeout: Timeout.none);

  // Run some of the microbenchmarks with tracing enabled but each category
  // disabled to measure the overhead of a trace event with the category turned
  // off.
  test('fuchsia_microbenchmarks_tracing_categories_disabled', () async {
    final helper = await PerfTestHelper.make();

    final List<File> resultsFiles = [];
    for (var process = 0; process < processRuns; ++process) {
      final traceSession = await helper.performance.initializeTracing(
          categories: ['nonexistent_category'], bufferSize: 36);
      await traceSession.start();

      final resultsFile = await helper.runTestComponentReturningResultsFile(
          packageName: 'fuchsia_microbenchmarks',
          componentName: 'fuchsia_microbenchmarks.cm',
          commandArgs: '-p --quiet'
              ' --out ${PerfTestHelper.componentOutputPath}'
              ' --runs $iterationsPerTestPerProcess'
              ' --filter $filterRegex --enable-tracing',
          resultsFileSuffix: '_process$process');
      addTestSuiteSuffix(resultsFile, '.tracing_categories_disabled');
      resultsFiles.add(resultsFile);

      await traceSession.stop();

      const testName = 'fuchsia_microbenchmarks_tracing_categories_disabled';
      final fxtTraceFile = await traceSession.terminateAndDownload(testName);
      final jsonTraceFile = await helper.performance
          .convertTraceFileToJson(_trace2jsonPath, fxtTraceFile);

      // All the real tracing categories are disabled, so we should
      // get no trace events.
      final Model model = await createModelFromFile(jsonTraceFile);
      expect(getAllEvents(model), []);
    }
    await helper.processResultsSummarized(resultsFiles,
        expectedMetricNamesFile:
            'fuchsia.microbenchmarks.tracing_categories_disabled.txt');
  }, timeout: Timeout.none);

  // --- Rust Trace Library Benchmarks
  //
  // We take a similar approach with the rust benchmarks. The library currently calls into the c
  // trace bindings, so our aim here is to ensure we aren't accidentally adding significant overhead
  // in the translation layer.
  //
  // These benchmarks are in a separate rust based binary, so we run 2 variants:
  // - Tracing disabled
  // - Tracing enabled, but "benchmark" category disabled
  //
  // TODO(b/295183613): Add a benchmark with tracing enabled. Currently the buffer fills up
  // instantly and then trace manager attempts to empty the buffer during the run, blocking its
  // async loop. At the same time sl4f tries to block on stopping the trace but doesn't try to read
  // the trace buffer resulting in a deadlock. Once that's done, it should be as easy as copying
  // this benchmark and changing the categories enabled to 'benchmark'.
  test('fuchsia_microbenchmarks_tracing_rust_categories_disabled', () async {
    final helper = await PerfTestHelper.make();
    final traceSession = await helper.performance
        .initializeTracing(categories: ['nonexistent_category'], bufferSize: 1);
    await traceSession.start();

    final resultsFile = await helper.runTestComponentReturningResultsFile(
        packageName: 'rust_trace_events_benchmarks',
        componentName: 'trace_events.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        resultsFileSuffix: '_categories_disabled');

    await traceSession.stop();
    await traceSession.terminateAndDownloadAsBytes();
    addTestSuiteSuffix(resultsFile, '.tracing_categories_disabled');

    await helper.processResultsSummarized([resultsFile],
        expectedMetricNamesFile:
            'fuchsia.trace_records.rust.tracing_categories_disabled.txt');
  }, timeout: Timeout.none);

  test('fuchsia_microbenchmarks_tracing_rust_tracing_disabled', () async {
    final helper = await PerfTestHelper.make();
    final resultsFile = await helper.runTestComponentReturningResultsFile(
        packageName: 'rust_trace_events_benchmarks',
        componentName: 'trace_events.cm',
        commandArgs: PerfTestHelper.componentOutputPath,
        resultsFileSuffix: '_tracing_disabled');

    addTestSuiteSuffix(resultsFile, '.tracing_disabled');
    await helper.processResultsSummarized([resultsFile],
        expectedMetricNamesFile:
            'fuchsia.trace_records.rust.tracing_disabled.txt');
  }, timeout: Timeout.none);
}
