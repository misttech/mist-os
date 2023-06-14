// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'dart:io';

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('socket_benchmarks', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.loopback.txt');
  }, timeout: Timeout.none);

  test('socket_benchmarks_with_fast_udp', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks-with-fast-udp.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.loopback.fastudp.txt');
  }, timeout: Timeout.none);

  test('socket_benchmarks_with_netstack3', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks-with-netstack3.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile:
            'fuchsia.network.socket.loopback.netstack3.txt');
  }, timeout: Timeout.none);

  test('socket_benchmarks_with_fake_netstack', () async {
    await runTestComponent(
        packageName: 'socket-benchmarks-tests',
        componentName: 'socket-benchmarks-with-fake-netstack.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile:
            'fuchsia.network.socket.loopback.fake_netstack.txt');
  }, timeout: Timeout.none);

  test('udp_serde_benchmarks', () async {
    await runTestComponent(
        packageName: 'udp-serde-benchmarks',
        componentName: 'udp-serde-benchmarks.cm',
        commandArgs: '-p --quiet --out ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.udp_serde.txt');
  }, timeout: Timeout.none);

  test('resource_usage_benchmarks', () async {
    await runTestComponent(
        packageName: 'resource-usage-benchmarks',
        componentName: 'resource-usage-benchmark-netstack2.cm',
        commandArgs: '-p',
        expectedMetricNamesFile: 'fuchsia.netstack.resource_usage.txt');
  }, timeout: Timeout.none);

  test('resource_usage_benchmarks_with_netstack3', () async {
    await runTestComponent(
        packageName: 'resource-usage-benchmarks',
        componentName: 'resource-usage-benchmark-netstack3.cm',
        commandArgs: '-p',
        expectedMetricNamesFile:
            'fuchsia.netstack.resource_usage.netstack3.txt');
  }, timeout: Timeout.none);
}
