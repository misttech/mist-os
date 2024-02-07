// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

// Helper code for setting up SL4F, running performance tests, and
// uploading the tests' results to the Catapult performance dashboard.

import 'package:logging/logging.dart';
import 'package:sl4f/sl4f.dart' as sl4f;
import 'package:test/test.dart';

void enableLoggingOutput() {
  // This is necessary to get information about the commands the tests have
  // run, and to get information about what they outputted on stdout/stderr
  // if they fail.
  Logger.root
    ..level = Level.ALL
    ..onRecord.listen((rec) => print('[${rec.level}]: ${rec.message}'));
}

class PerfTestHelper {
  late sl4f.Sl4f sl4fDriver;
  late sl4f.Performance performance;
  late sl4f.Dump dump;
  late sl4f.Component component;

  // The simpler test cases depend on only on SSH and not on the SL4F
  // server.  These can be run without starting the SL4F server.  That
  // allows these tests to be run from a products/*.gni config that
  // doesn't include the SL4F server.  It also allows these tests to
  // be run locally on QEMU without doing the extra networking setup
  // that is required for making TCP connections from the host to the
  // SL4F server.
  Future<void> setUp({bool requiresSl4fServer = true}) async {
    sl4fDriver = sl4f.Sl4f.fromEnvironment();
    if (requiresSl4fServer) {
      await sl4fDriver.startServer();
      addTearDown(() async {
        await sl4fDriver.stopServer();
        sl4fDriver.close();
      });
    }
    performance = sl4f.Performance(sl4fDriver);
    dump = sl4f.Dump();
    component = sl4f.Component(sl4fDriver);
  }

  static Future<PerfTestHelper> make() async {
    final helper = PerfTestHelper();
    await helper.setUp();
    return helper;
  }
}
