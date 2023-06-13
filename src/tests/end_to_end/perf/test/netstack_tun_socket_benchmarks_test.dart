// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:test/test.dart';

import 'helpers.dart';

void main() {
  enableLoggingOutput();

  test('tun_socket_benchmarks', () async {
    await runTestComponent(
        packageName: 'tun-socket-benchmarks',
        componentName: 'tun-socket-benchmarks-ns2.cm',
        commandArgs: '--output-fuchsiaperf ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.tun.txt');
  }, timeout: Timeout.none);

  test('tun_socket_benchmarks_with_netstack3', () async {
    await runTestComponent(
        packageName: 'tun-socket-benchmarks',
        componentName: 'tun-socket-benchmarks-ns3.cm',
        commandArgs: '--output-fuchsiaperf ${PerfTestHelper.componentOutputPath}',
        expectedMetricNamesFile: 'fuchsia.network.socket.tun.netstack3.txt');
  }, timeout: Timeout.none);
}
