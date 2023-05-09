// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:fxtest/fxtest.dart';
import 'package:io/ansi.dart';

import 'exit_code.dart';

/// Main entry-point for all Fuchsia tests, both host and on-device.
///
/// [FuchsiaTestCommand] receives a combination of test names and feature flags,
/// and due to the nature of Fuchsia tests, needs to run each passed test name
/// against the full set of feature flags. For example, if the way to invoke
/// this command is `fx test <...args>`, and the developer executes
/// `... test1 test2 -flag1 -flag2`, the desired behavior for our user lies
/// behind pretending the user entered `... test1 -flag1 -flag2` and
/// `... test2 -flag1 flag2` separately. Note that the individual tests indicated
/// by these two imaginary commands are then rolled back into one singular test
/// suite, so end users will be none the wiser.
///
/// The following is a high level road map of how [FuchsiaTestCommand] works:
///
///  - Parse commands into testNames and flags
///  - Produce a list of each individual `testName` paired with all provided flags
///     - loop over this list `testName` and flag combinations, matching tests
///       out of `out/default/tests.json`
///     - Load each matching test's raw JSON into a [TestDefinition] instance which
///       handles determining all the runtime invocation details for a given test
///  - Loop over the list of [TestDefinition] instances, listening to emitted
///    [TestEvent] instances with a [FormattedOutput] class
///     - Every time a [TestEvent] instance is emitted, our [FormattedOutput]
///       class captures it and flushes appropriate text to [stdout].
///  - Once the [TestDefinition] list has been completely processed, flush any
///    captured stderr from individual test processes to show errors / stacktraces
class FuchsiaTestCommand {
  // ignore: close_sinks
  final _eventStreamController = StreamController<TestEvent>();

  final AnalyticsReporter analyticsReporter;

  /// Bundle of configuration options for this invocation.
  final TestsConfig testsConfig;

  /// Translators between [TestEvent] instances and output for the user.
  final List<OutputFormatter> outputFormatters;

  /// Function that yields disposable wrappers around tests and [Process]
  /// instances.
  final TestRunner Function(TestsConfig) testRunnerBuilder;

  /// Helper which verifies that we actually want to run tests (not a dry run,
  /// for example) and are set up to even be successful (device and package
  /// server are available, for example).
  final Checklist checklist;

  final ExitCodeSetter _exitCodeSetter;

  /// Used to create any new directories needed to house test output / artifacts.
  final DirectoryBuilder directoryBuilder;

  int _numberOfTests;

  late StreamSubscription<TestEvent> _streamSubscription;

  /// Used to obtain package hashes for component tests. Lazily loaded so that
  /// host tests do not rely on a package repository.
  PackageRepository? _packageRepository;

  PackageRepository? get packageRepository => _packageRepository;

  FuchsiaTestCommand({
    required this.analyticsReporter,
    required this.outputFormatters,
    required this.checklist,
    required this.testsConfig,
    required this.testRunnerBuilder,
    required this.directoryBuilder,
    ExitCodeSetter? exitCodeSetter,
  })  : _exitCodeSetter = exitCodeSetter ?? setExitCode,
        _numberOfTests = 0 {
    if (outputFormatters.isEmpty) {
      throw AssertionError('Must provide at least one OutputFormatter');
    }
    _streamSubscription = stream.listen((output) {
      for (var formatter in outputFormatters) {
        formatter.update(output);
      }
    });
  }

  factory FuchsiaTestCommand.fromConfig(
    TestsConfig testsConfig, {
    required TestRunner Function(TestsConfig) testRunnerBuilder,
    DirectoryBuilder? directoryBuilder,
    ExitCodeSetter? exitCodeSetter,
    OutputFormatter? outputFormatter,
  }) {
    var _outputFormatter =
        outputFormatter ?? OutputFormatter.fromConfig(testsConfig);
    var _fileFormatter = FileFormatter.fromConfig(testsConfig);
    return FuchsiaTestCommand(
      analyticsReporter: testsConfig.flags.dryRun
          ? AnalyticsReporter.noop()
          : AnalyticsReporter(fxEnv: testsConfig.fxEnv),
      checklist: PreChecker.fromConfig(
        testsConfig,
        eventSink: _outputFormatter.update,
      ),
      directoryBuilder:
          directoryBuilder ?? (path, {required recursive}) => null,
      outputFormatters: [
        _outputFormatter,
        if (_fileFormatter != null) _fileFormatter
      ],
      testRunnerBuilder: testRunnerBuilder,
      testsConfig: testsConfig,
      exitCodeSetter: exitCodeSetter,
    );
  }

  Stream<TestEvent> get stream => _eventStreamController.stream;

  void emitEvent(TestEvent event) {
    if (!_eventStreamController.isClosed) {
      _eventStreamController.sink.add(event);
    }
  }

  Future<void> _flushOutput() async {
    if (!_eventStreamController.isClosed) {
      _streamSubscription.pause();
      for (var formatter in outputFormatters) {
        await formatter.flush();
      }
      _streamSubscription.resume();
    }
  }

  Future<void> dispose() async {
    var fut = _streamSubscription.asFuture();
    await _eventStreamController.close();
    await fut;
    for (var formatter in outputFormatters) {
      await formatter.close();
    }
  }

  Future<void> runTestSuite([TestsManifestReader? manifestReader]) async {
    manifestReader ??= TestsManifestReader();
    advertiseLogFile();
    var parsedManifest = await readManifest(manifestReader);

    manifestReader.reportOnTestBundles(
      userFriendlyBuildDir: testsConfig.fxEnv.userFriendlyOutputDir!,
      eventEmitter: emitEvent,
      parsedManifest: parsedManifest,
      testsConfig: testsConfig,
    );

    if (parsedManifest.testBundles.isEmpty) {
      _exitCodeSetter(noTestFoundExitCode);
      return noMatchesHelp(
        manifestReader: manifestReader,
        testDefinitions: parsedManifest.testDefinitions,
        testsConfig: testsConfig,
      );
    } else if (parsedManifest.unusedConfigs?.isNotEmpty ?? false) {
      _exitCodeSetter(noTestFoundExitCode);
      return unusedConfigsHelp(
        manifestReader: manifestReader,
        testDefinitions: parsedManifest.testDefinitions,
        unusedConfigs: parsedManifest.unusedConfigs!,
      );
    }

    if (testsConfig.flags.shouldRandomizeTestOrder) {
      parsedManifest.testBundles.shuffle();
    }

    if (testsConfig.flags.shouldRebuild) {
      Set<String> buildArgs = await TestBundle.calculateMinimalBuildTargets(
          testsConfig, parsedManifest.testBundles);
      emitEvent(TestInfo(testsConfig
          .wrapWith('> fx build ${buildArgs.join(' ')}', [green, styleBold])));
      try {
        await fxCommandRun(testsConfig.fxEnv.fx, 'build', buildArgs.toList());
      } on FxRunException {
        emitEvent(FatalError(
            '\'fx test\' could not perform a successful build. Try to run \'fx build\' manually or use the \'--no-build\' flag'));
        _exitCodeSetter(failureExitCode);
        return;
      }

      // FIXME(http://fxbug.dev/107343): When running `fx test` with incremental
      // publishing, it's possible we could trigger a test to run before the
      // incremental publisher has published all the test packages we just
      // built. When this happens, the test could end up running the old tests.
      //
      // Ideally the incremental publisher would have a way to block until the
      // publishing happened. As a stopgap, this section will explicitly publish
      // all the test package manifests. This shouldn't corrupt the repository
      // because `package-tool` grabs the repository lock before we make any
      // changes to it.
      //
      // However, while this protects us from starting a test before the test
      // packages are published, we still could run into some races. It's
      // possible components in the test package could depend on other packages,
      // which is not visible in the `tests.json`. If those packages also were
      // dirtied, then we could start the test before those packages were
      // published.
      //
      // So long term we still need a way to block until the incremental
      // publisher finishes publishing those packages before we start start the
      // tests.
      if (testsConfig.fxEnv.outputDir != null) {
        String outputDir = testsConfig.fxEnv.outputDir!;

        if (TestBundle.hasDevicePackages(parsedManifest.testBundles) &&
            (testsConfig.fxEnv
                    .isFeatureEnabled('fxtest_auto_publishes_packages') ||
                testsConfig.fxEnv.isFeatureEnabled('incremental') ||
                testsConfig.fxEnv.isFeatureEnabled('incremental_new') ||
                testsConfig.fxEnv.isFeatureEnabled('incremental_legacy'))) {
          String amberFilesDir = outputDir + "/amber-files";

          List<String> args = [
            "host-tool",
            "package-tool",
            "repository",
            "publish",
            "--trusted-root",
            amberFilesDir + "/repository/root.json",
            "--ignore-missing-packages",
            "--time-versioning",
          ];

          int? delivery_blob_type = await readDeliveryBlobType(
              outputDir + "/delivery_blob_config.json");
          if (delivery_blob_type != null) {
            args.addAll(["--delivery-blob-type", "${delivery_blob_type}"]);
          }

          args.add("--package-list");
          args.add(outputDir + "/all_package_manifests.list");
          args.add(amberFilesDir);

          emitEvent(TestInfo(testsConfig
              .wrapWith('> fx ${args.join(' ')}', [green, styleBold])));

          try {
            await Process.start(testsConfig.fxEnv.fx, args,
                    mode: ProcessStartMode.inheritStdio,
                    workingDirectory: outputDir)
                .then((Process process) async {
              final _exitCode = await process.exitCode;
              if (_exitCode != 0) {
                throw FxRunException(
                    'Failed to run fx ${args.join(' ')}', _exitCode);
              }
            });
          } on FxRunException {
            emitEvent(FatalError(
                '\'fx test\' could not perform a successful publish. Try to run \'fx build\' manually or use the \'--no-build\' flag'));
            _exitCodeSetter(failureExitCode);
            return;
          }
        }
      }

      // Re-parse the manifest in case it has changed as a side effect of building
      parsedManifest = await readManifest(manifestReader);
    }

    try {
      // Let the output formatter know that we're done parsing and
      // emitting preliminary events
      emitEvent(BeginningTests());
      await runTests(parsedManifest.testBundles);
    } on FailFastException catch (_) {
      // Non-zero exit code indicates generic but fatal failure
      _exitCodeSetter(failureExitCode);
    }
    emitEvent(AllTestsCompleted());
  }

  Future<ParsedManifest> readManifest(
    TestsManifestReader manifestReader,
  ) async {
    List<TestDefinition> testDefinitions = await manifestReader.loadTestsJson(
        buildDir: testsConfig.fxEnv.outputDir!,
        fxLocation: testsConfig.fxEnv.fx,
        manifestFileName: 'tests.json',
        testComponentManifestFileName: 'test_components.json');
    return manifestReader.aggregateTests(
      eventEmitter: emitEvent,
      matchLength: testsConfig.flags.matchLength,
      testBundleBuilder: testBundleBuilder,
      testDefinitions: testDefinitions,
      testsConfig: testsConfig,
    );
  }

  Future<int?> readDeliveryBlobType(
    String path,
  ) async {
    File file = await File(path);
    if (!await file.exists()) {
      return null;
    }
    final delivery_blob_config = jsonDecode(await file.readAsString());
    return delivery_blob_config["type"];
  }

  List<String> fuzzyMatchArgsForConfig({
    required TestsConfig testsConfig,
  }) {
    final List<String> ret = [];
    if (testsConfig.flags.isVerbose && !testsConfig.flags.allOutput) {
      ret.add('--debug');
    }
    return ret;
  }

  void noMatchesHelp({
    required TestsManifestReader manifestReader,
    required List<TestDefinition> testDefinitions,
    required TestsConfig testsConfig,
  }) {
    emitEvent(GeneratingHintsEvent());
    emitEvent(TestInfo(
      testsConfig.wrapWith(
          'Could not find any tests to run with the '
          'arguments you provided.',
          [lightYellow]),
    ));

    final List<String> fuzzyMatchArgs =
        fuzzyMatchArgsForConfig(testsConfig: testsConfig);

    var manifestOfHints = manifestReader.aggregateTests(
      comparer: FuzzyComparer(threshold: testsConfig.flags.fuzzyThreshold),
      eventEmitter: (TestEvent event) => null,
      matchLength: testsConfig.flags.matchLength,
      testBundleBuilder: testBundleBuilder,
      testDefinitions: testDefinitions,
      testsConfig: testsConfig,
    );
    if (manifestOfHints.testBundles.isNotEmpty &&
        testsConfig.flags.shouldShowSuggestions) {
      manifestOfHints.testBundles.sort(
        (TestBundle bundle1, TestBundle bundle2) {
          return bundle1.confidence.compareTo(bundle2.confidence);
        },
      );
      var hints = manifestOfHints.testBundles.length > 1 ? 'hints' : 'hint';
      emitEvent(TestInfo(
        'Did you mean... (${manifestOfHints.testBundles.length} $hints)?',
        requiresPadding: false,
      ));
      for (TestBundle bundle in manifestOfHints.testBundles) {
        emitEvent(TestInfo(
          ' -- ${bundle.testDefinition.name}',
          requiresPadding: false,
        ));
      }

      // Omit test file results so we do not duplicate the above results.
      fuzzyMatchArgs.add('--omit-test-file');
      final permutation = testsConfig.permutations.first;
      emitEvent(TestInfo(
          'For ${permutation.name()}, did you mean any of the following?'));
      fxCommandRun(testsConfig.fxEnv.fx, 'search-tests',
          fuzzyMatchArgs + [permutation.name()]);
    } else {
      emitEvent(TestInfo(
        'Make sure this test is transitively in your \'fx set\' arguments. See https://fuchsia.dev/fuchsia-src/development/testing/faq for more information.',
        requiresPadding: false,
      ));

      if (testsConfig.flags.shouldShowSuggestions) {
        final permutation = testsConfig.permutations.first;
        emitEvent(TestInfo(
            'For ${permutation.name()}, did you mean any of the following?'));
        fxCommandRun(testsConfig.fxEnv.fx, 'search-tests',
            fuzzyMatchArgs + [permutation.name()]);
      }
    }
  }

  void unusedConfigsHelp({
    required TestsManifestReader manifestReader,
    required List<TestDefinition> testDefinitions,
    required List<PermutatedTestsConfig> unusedConfigs,
  }) {
    emitEvent(GeneratingHintsEvent());
    String unusedConfigsString = unusedConfigs
        .map(
            (config) => config.testNameGroup?.map((name) => name.arg).join(','))
        .join(';');
    emitEvent(TestInfo(
      testsConfig.wrapWith(
          'Could not find any tests that matched these arguments: $unusedConfigsString.',
          [lightYellow]),
    ));
    emitEvent(TestInfo(
      'Make sure this test is transitively in your \'fx set\' arguments. See https://fuchsia.dev/fuchsia-src/development/testing/faq for more information.',
      requiresPadding: false,
    ));

    if (testsConfig.flags.shouldShowSuggestions) {
      final List<String> fuzzyMatchArgs =
          fuzzyMatchArgsForConfig(testsConfig: testsConfig);
      final permutation = unusedConfigs.first;
      emitEvent(TestInfo(
          'For ${permutation.name()}, did you mean any of the following?'));
      fxCommandRun(testsConfig.fxEnv.fx, 'search-tests',
          fuzzyMatchArgs + [permutation.name()]);
    }
  }

  TestBundle testBundleBuilder(
    TestDefinition testDefinition, [
    double? confidence,
  ]) =>
      TestBundle.build(
        confidence: confidence ?? 1,
        directoryBuilder: directoryBuilder,
        fxPath: testsConfig.fxEnv.fx,
        realtimeOutputSink: (String val) => emitEvent(TestOutputEvent(val)),
        timeElapsedSink: (Duration duration, String cmd, String output) =>
            emitEvent(TimeElapsedEvent(duration, cmd, output)),
        testRunnerBuilder: testRunnerBuilder,
        testDefinition: testDefinition,
        testsConfig: testsConfig,
        workingDirectory: testsConfig.fxEnv.outputDir!,
      );

  Future<bool> maybeAddPackageHash(TestBundle testBundle) async {
    // TODO: This should not require checking if `buildDir != null`, as that is
    // a temporary workaround to get tests passing on CQ. The correct
    // implementation is to abstract file-reading just as we have process-launching.
    if (testsConfig.flags.shouldUsePackageHash &&
        testBundle.testDefinition.packageUrl != null &&
        testsConfig.fxEnv.outputDir != null) {
      _packageRepository ??= await PackageRepository.fromManifest(
          buildDir: testsConfig.fxEnv.outputDir!);
      if (_packageRepository == null) {
        emitEvent(TestResult.failedPreprocessing(
          message:
              'Package repository is not available. Run "fx serve-updates" again or use the "--no-use-package-hash" flag.',
          testName: testBundle.testDefinition.name,
        ));
        return false;
      } else {
        String packageName = testBundle.testDefinition.packageUrl!.packageName;
        if (_packageRepository![packageName] == null) {
          emitEvent(TestResult.failedPreprocessing(
              message:
                  'Package $packageName is not in the package repository, check if it was correctly built or use the "--no-use-package-hash" flag.',
              testName: testBundle.testDefinition.name));
          return false;
        }
        testBundle.testDefinition.hash =
            _packageRepository![packageName]!.merkle;
      }
    }
    return true;
  }

  Future<void> runTests(List<TestBundle> testBundles) async {
    // Enforce a limit
    var _testBundles = testsConfig.flags.limit > 0 &&
            testsConfig.flags.limit < testBundles.length
        ? testBundles.sublist(0, testsConfig.flags.limit)
        : testBundles;
    if (!testsConfig.flags.infoOnly &&
        !await checklist.isDeviceReady(_testBundles)) {
      emitEvent(FatalError('Device is not ready for running device tests'));
      _exitCodeSetter(failureExitCode);
      return;
    }

    // set merkle root hash on component tests
    for (TestBundle testBundle in _testBundles) {
      if (!testsConfig.flags.infoOnly &&
          !await maybeAddPackageHash(testBundle)) {
        _exitCodeSetter(failureExitCode);
        continue;
      }
      await testBundle.run().forEach((TestEvent event) {
        emitEvent(event);
        if (event is FatalError) {
          _exitCodeSetter(failureExitCode);
        } else if (event is TestResult && !event.isSuccess) {
          _exitCodeSetter(event.exitCode);
        }
      });
      _numberOfTests += 1;
    }
  }

  /// Function guaranteed to be called at the end of execution, whether that is
  /// natural or the result of a SIGINT.
  Future<void> cleanUp() async {
    await _reportAnalytics();
    await _flushOutput();
  }

  Future<void> _reportAnalytics() async {
    final bool _actuallyRanTests = _numberOfTests > 0;
    if (!testsConfig.flags.dryRun && _actuallyRanTests) {
      await analyticsReporter.report(
        subcommand: 'test',
        action: 'number',
        label: '$_numberOfTests',
      );
    }
  }

  void advertiseLogFile() {
    for (var outputFormatter in outputFormatters) {
      if (outputFormatter is FileFormatter) {
        // ignore: avoid_as
        (outputFormatter.buffer.stdout as FileStandardOut).initPath();
        // ignore: avoid_as
        var path = (outputFormatter.buffer.stdout as FileStandardOut).path;
        emitEvent(TestInfo(testsConfig.wrapWith(
          'Logging all output to: $path\n'
          'Use the `--logpath` argument to specify a log location or '
          '`--no-log` to disable\n',
          [darkGray],
        )));
      }
    }

    if (testsConfig.flags.allOutput) {
      emitEvent(TestInfo(testsConfig.wrapWith(
        'Printing all output to console (`-o/--output` specified)\n',
        [darkGray],
      )));
    } else if (testsConfig.flags.slowThreshold > 0) {
      emitEvent(TestInfo(testsConfig.wrapWith(
        'Output will be printed to the console for tests taking more than'
        ' ${testsConfig.flags.slowThreshold} seconds.\n'
        'To change the timeout threshold, specify the `-s/--slow` flag.\n'
        'To show all output, specify the `-o/--output` flag.\n',
        [darkGray],
      )));
    } else {
      emitEvent(TestInfo(testsConfig.wrapWith(
        'Output will not be printed to the console.\n'
        'To show all output, specify the `-o/--output` flag.\n',
        [darkGray],
      )));
    }
  }
}
