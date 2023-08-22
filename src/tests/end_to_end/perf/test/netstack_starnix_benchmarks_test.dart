// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'dart:io';

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  // TODO(https://issuetracker.google.com/296292263): The run count can be
  // doubled once the pathological slow-down has been addressed, because the
  // bulk of runtime is spent in these cases for NS3 and we expect runtime to
  // approximately halve.
  //
  // Reduce the number of runs for the starnix benchmarks to avoid hitting a
  // timeout in Infra.
  const int starnixRunCount = 120;

  test('loopback_socket_benchmarks_starnix', () async {
    await runTestComponent(
        packageName: 'loopback-socket-benchmarks-starnix',
        componentName: 'loopback-socket-benchmarks-starnix.cm',
        realm: "/core/testing:starnix-tests",
        commandArgs:
            '-p --runs $starnixRunCount --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.loopback.starnix.txt');
  }, timeout: Timeout.none);

  test('loopback_socket_benchmarks_starnix_with_netstack3', () async {
    await runTestComponent(
        packageName: 'loopback-socket-benchmarks-starnix-with-netstack3',
        componentName: 'loopback-socket-benchmarks-starnix-with-netstack3.cm',
        realm: "/core/testing:starnix-tests",
        commandArgs:
            '-p --runs $starnixRunCount --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile:
            'fuchsia.network.socket.loopback.starnix.netstack3.txt');
  }, timeout: Timeout.none);

  test('loopback_socket_benchmarks_starnix_with_fake_netstack', () async {
    await runTestComponent(
        packageName: 'loopback-socket-benchmarks-starnix-with-fake-netstack',
        componentName:
            'loopback-socket-benchmarks-starnix-with-fake-netstack.cm',
        realm: "/core/testing:starnix-tests",
        commandArgs:
            '-p --runs $starnixRunCount --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile:
            'fuchsia.network.socket.loopback.starnix.fake_netstack.txt');
  }, timeout: Timeout.none);
}
