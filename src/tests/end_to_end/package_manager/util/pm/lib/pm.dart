// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/84961): Fix null safety and remove this language version.
// @dart=2.9

import 'dart:async';
import 'dart:convert';
import 'dart:core';
import 'dart:io';

import 'package:async/async.dart';
import 'package:logging/logging.dart';
import 'package:net/curl.dart';
import 'package:net/ports.dart';
import 'package:path/path.dart' as path;
import 'package:quiver/core.dart' show Optional;
import 'package:retry/retry.dart';
import 'package:sl4f/sl4f.dart' as sl4f;
import 'package:test/test.dart';

class PackageManagerRepo {
  final sl4f.Sl4f _sl4fDriver;
  final String _ffxPath;
  final String _ffxIsolateDir;
  final String _repoPath;
  final Logger _log;
  Optional<int> _servePort;

  Optional<int> getServePort() => _servePort;
  String getRepoPath() => _repoPath;

  PackageManagerRepo._create(this._sl4fDriver, this._ffxPath,
      this._ffxIsolateDir, this._repoPath, this._log) {
    _servePort = Optional.absent();
  }

  static Future<PackageManagerRepo> initRepo(
      sl4f.Sl4f sl4fDriver, String ffxPath, Logger log) async {
    var repoPath = (await Directory.systemTemp.createTemp('repo')).path;
    var ffxIsolateDir =
        (await Directory.systemTemp.createTemp('ffx_isolate_dir')).path;
    return PackageManagerRepo._create(
        sl4fDriver, ffxPath, ffxIsolateDir, repoPath, log);
  }

  /// Create new repo using `ffx repository create`.
  ///
  /// Uses this command:
  /// `ffx repository create <repo path>`
  Future<void> ffxRepositoryCreate() async {
    _log.info('Initializing repo: $_repoPath');

    await ffx(['repository', 'create', _repoPath]);
  }

  /// Publish an archive to a repo using `ffx repository publish`.
  ///
  /// Uses this command:
  /// `ffx repository publish --package-archive <archive path> <repo path>`
  Future<void> ffxRepositoryPublish(String archivePath) async {
    _log.info('Publishing $archivePath to repo.');
    await ffx(
        ['repository', 'publish', '--package-archive', archivePath, _repoPath]);
  }

  /// Create archive for a given manifest using `ffx package archive create`.
  ///
  /// Uses this command:
  /// `ffx package archive create <manifest path> --out <archivePath> --root-dir <rootDirectory>`
  Future<void> ffxPackageArchiveCreate(
      String packageManifestPath, String archivePath) async {
    _log.info('Creating archive from a given package manifest.');
    final rootDirectory = Platform.script.resolve('runtime_deps').toFilePath();
    await ffx(
      [
        'package',
        'archive',
        'create',
        packageManifestPath,
        '--out',
        archivePath,
        '--root-dir',
        rootDirectory
      ],
    );
  }

  /// Attempts to start the `ffx repository server` process.
  ///
  /// `curl` will be used as an additional check for whether `ffx repository
  /// server` has successfully started.
  ///
  /// Returns `true` if serve startup was successful.
  Future<bool> tryServe(String repoName, int port) async {
    await stopServer();
    await ffx(
        ['repository', 'add-from-pm', _repoPath, '--repository', repoName]);

    final start_result = await Process.run(_ffxPath + "/ffx", [
      '--config',
      'ffx.subtool-search-paths=' + _ffxPath,
      '--isolate-dir',
      _ffxIsolateDir,
      'repository',
      'server',
      'start',
      '--address',
      '[::]:$port'
    ]);
    if (start_result.exitCode != 0) {
      _log.info('ffx repository server start failed: ${start_result.stderr}');
      return false;
    }

    final status = await serverStatus();
    expect(status['state'], 'running');

    _servePort = Optional.of(Uri.parse('http://' + status['address']).port);

    if (port != 0) {
      expect(_servePort.value, port);
    }
    _log.info('Wait until serve responds to curl.');
    final curlStatus = await retryWaitForCurlHTTPCode(
        ['http://localhost:${_servePort.value}/$repoName/targets.json'], 200,
        logger: _log);
    _log.info('curl return code: $curlStatus');
    return curlStatus == 0;
  }

  /// Start a package server using `ffx repository server` with serve-selected
  /// port.
  ///
  /// Passes in `--address [::]:0` to tell `ffx repository` to choose its own
  /// port.
  ///
  /// Does not return until the serve begins listening, or times out.
  ///
  /// Uses these commands:
  /// `ffx repository add-from-pm <repo path> --repository <repo name>`
  /// `ffx repository server start --address [::]:0`
  Future<void> startServer(String repoName) async {
    _log.info('Server is starting.');
    final retryOptions = RetryOptions(maxAttempts: 5);
    await retryOptions.retry(() async {
      if (!await tryServe(repoName, 0)) {
        throw Exception(
            'Attempt to bringup `ffx repository server` has failed.');
      }
    });
  }

  /// Start a package server using `ffx repository server` with our own port
  /// selection.
  ///
  /// Does not return until the serve begins listening, or times out.
  ///
  /// Uses these commands:
  /// `ffx repository add-from-pm <repo path> --repository <repo name>`
  /// `ffx repository server start --address [::]:<port number>`
  Future<void> startServerUnusedPort(String repoName) async {
    await getUnusedPort<bool>((unusedPort) async {
      _log.info('Serve is starting on port: $unusedPort');
      if (await tryServe(repoName, unusedPort)) {
        return true;
      }
      return null;
    });

    expect(_servePort.isPresent, isTrue);
  }

  Future<dynamic> serverStatus() async {
    return json.decode(
        await ffx(['--machine', 'json', 'repository', 'server', 'status']));
  }

  /// Register the repo in target using `ffx target repository register`.
  ///
  /// Uses this command:
  /// `ffx target repository register`
  Future<void> ffxTargetRepositoryRegister() async {
    await ffx(['target', 'repository', 'register']);
  }

  Future<String> ffx(List<String> args) async {
    final result = await Process.run(
        _ffxPath + "/ffx",
        [
              '--config',
              'ffx.subtool-search-paths=' + _ffxPath,
              '--isolate-dir',
              _ffxIsolateDir
            ] +
            args);
    expect(result.exitCode, 0,
        reason: '`ffx ${args.join(" ")}` failed: ' + result.stderr);
    return result.stdout;
  }

  /// Get the named component from the repo using `pkgctl resolve`.
  ///
  /// Uses this command:
  /// `pkgctl resolve <component URL>`
  Future<ProcessResult> pkgctlResolve(
      String msg, String url, int retCode) async {
    return _sl4fRun(msg, 'pkgctl resolve', [url], retCode);
  }

  /// Get the named component from the repo using `pkgctl resolve --verbose`.
  ///
  /// Uses this command:
  /// `pkgctl resolve --verbose <component URL>`
  Future<ProcessResult> pkgctlResolveV(
      String msg, String url, int retCode) async {
    return _sl4fRun(msg, 'pkgctl resolve --verbose', [url], retCode);
  }

  /// List repo sources using `pkgctl repo`.
  ///
  /// Uses this command:
  /// `pkgctl repo`
  Future<ProcessResult> pkgctlRepo(String msg, int retCode) async {
    return _sl4fRun(msg, 'pkgctl repo', [], retCode);
  }

  /// Remove a repo source using `pkgctl repo rm`.
  ///
  /// Uses this command:
  /// `pkgctl repo rm <repo name>`
  Future<ProcessResult> pkgctlRepoRm(
      String msg, String repoName, int retCode) async {
    return _sl4fRun(msg, 'pkgctl repo', ['rm $repoName'], retCode);
  }

  /// Replace dynamic rules using `pkgctl rule replace`.
  ///
  /// Uses this command:
  /// `pkgctl rule replace json <json>`
  Future<ProcessResult> pkgctlRuleReplace(
      String msg, String json, int retCode) async {
    return _sl4fRun(msg, 'pkgctl rule replace', ['json \'$json\''], retCode);
  }

  /// List redirect rules using `pkgctl rule list`.
  ///
  /// Uses this command:
  /// `pkgctl rule list`
  Future<ProcessResult> pkgctlRuleList(String msg, int retCode) async {
    return _sl4fRun(msg, 'pkgctl rule list', [], retCode);
  }

  /// List redirect rules using `pkgctl rule dump-dynamic`.
  ///
  /// Uses this command:
  /// `pkgctl rule dump-dynamic`
  Future<ProcessResult> pkgctlRuleDumpdynamic(String msg, int retCode) async {
    return _sl4fRun(msg, 'pkgctl rule dump-dynamic', [], retCode);
  }

  /// Get a package hash `pkgctl get-hash`.
  ///
  /// Uses this command:
  /// `pkgctl get-hash <package name>`
  Future<ProcessResult> pkgctlGethash(
      String msg, String package, int retCode) async {
    return _sl4fRun(msg, 'pkgctl', ['get-hash $package'], retCode);
  }

  Future<ProcessResult> _sl4fRun(
      String msg, String cmd, List<String> params, int retCode,
      {bool randomize = false}) async {
    _log.info(msg);
    if (randomize) {
      params.shuffle();
    }
    var cmdBuilder = StringBuffer()
      ..write(cmd)
      ..write(' ');
    for (var param in params) {
      cmdBuilder
        ..write(param)
        ..write(' ');
    }
    final response = await _sl4fDriver.ssh.run(cmdBuilder.toString());
    expect(response.exitCode, retCode);
    return response;
  }

  Future<bool> setupRepo(String farPath, String manifestPath) async {
    final archivePath =
        Platform.script.resolve('runtime_deps/$farPath').toFilePath();
    await Future.wait([
      ffxRepositoryCreate(),
      ffxPackageArchiveCreate(manifestPath, archivePath)
    ]);

    _log.info(
        'Publishing package from archive: $archivePath to repo: $_repoPath');
    await ffxRepositoryPublish(archivePath);

    return true;
  }

  Future<bool> setupServe(
      String farPath, String manifestPath, String repoName) async {
    await setupRepo(farPath, manifestPath);
    await startServerUnusedPort(repoName);
    return true;
  }

  Future<void> stopServer() async {
    final status = await serverStatus();
    if (status['state'] == 'running') {
      await ffx(['repository', 'server', 'stop']);
    }
  }

  Future<void> cleanup() async {
    await ffx(['daemon', 'stop']);
    await Future.wait([
      Directory(_repoPath).delete(recursive: true),
      Directory(_ffxIsolateDir).delete(recursive: true),
    ]);
  }
}
