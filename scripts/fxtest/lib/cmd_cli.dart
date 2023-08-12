// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:io';

import 'package:io/ansi.dart';
import 'package:args/args.dart';
import 'package:fxtest/fxtest.dart';
import 'package:fxutils/fxutils.dart';
import 'package:pedantic/pedantic.dart';

/// Translator for command line arguments into [FuchsiaTestCommand] primitives.
class FuchsiaTestCommandCli {
  /// Callable that prints help/usage information for when the user passes
  /// invalid arguments or "--help".
  final Function(ArgParser) usage;

  /// Fully-hydrated object containing answers to every runtime question.
  /// Derivable from the set of raw arguments passed in by the user.
  late final TestsConfig testsConfig;

  /// The underlying class which does all the work.
  FuchsiaTestCommand? _cmd;

  /// Handle to run operation.
  Future? _runFuture;

  /// Used to create any new directories needed to house test output / artifacts.
  final DirectoryBuilder? directoryBuilder;

  final IFxEnv fxEnv;

  FuchsiaTestCommandCli(
    List<String> rawArgs, {
    required this.usage,
    required this.fxEnv,
    this.directoryBuilder,
  }) {
    testsConfig = TestsConfig.fromRawArgs(
      rawArgs: rawArgs,
      // When running real tests, turn on logging. Passing `--no-log` explicitly
      // will still override this.
      // The `null` value is not a falsy indicator - it is because `--log` is a
      // flag and thus does not accept a value.
      defaultRawArgs: {'--log': null},
      fxEnv: fxEnv,
    );
  }

  Future<bool> preRunChecks(
    Function(Object) stdoutWriter, {
    ProcessLauncher? processLauncher,
  }) async {
    if (testsConfig.testArguments.parsedArgs['help']) {
      usage(fxTestArgParser);
      return false;
    }
    if (testsConfig.testArguments.parsedArgs['printtests']) {
      processLauncher ??= ProcessLauncher();
      ProcessResult result = await processLauncher.run('cat', ['tests.json'],
          workingDirectory: fxEnv.outputDir);
      stdoutWriter(result.stdout);
      return false;
    }

    // This command uses extensive fx re-entry, so it's good to make sure fx
    // is actually located where we expect
    final fxFile = File(testsConfig.fxEnv.fx);
    if (!fxFile.existsSync()) {
      throw MissingFxException();
    }

    return true;
  }

  Future<void> run() async {
    print('Want to try something new? 🤔\n' +
        'The rewrite of `fx test` is in Fishfood. 🐟\n' +
        'Learn more: https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/scripts/fxtest/rewrite');

    _cmd = createCommand();

    //register a listener for when run completes, which resolves the
    // stdout future.
    _runFuture = _cmd!.runTestSuite(TestsManifestReader()).then((_) async {
      // Once the actual command finishes without problems, close the output
      // formatters.
      await _cmd!.dispose();
    });

    // Register a listener for when the `stdout` closes.
    try {
      await Future.wait(
        _cmd!.outputFormatters.map((var f) => f.stdOutClosedFuture),
        eagerError: true,
      );
    } on Exception {
      await _runFuture!;
      if (exitCode == 0) {
        rethrow;
      } else {
        throw OutputClosedException(exitCode);
      }
    }
    await _runFuture!;
  }

  FuchsiaTestCommand createCommand() => FuchsiaTestCommand.fromConfig(
        testsConfig,
        directoryBuilder: directoryBuilder,
        testRunnerBuilder: (TestsConfig testsConfig) => SymbolizingTestRunner(
          fx: testsConfig.fxEnv.fx,
        ),
      );

  Future<void> terminateEarly() async {
    _cmd?.emitEvent(AllTestsCompleted());
    // Complete run future
    await _runFuture;
  }

  Future<void> cleanUp() async {
    await _cmd?.cleanUp();
  }
}
