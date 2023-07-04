// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'package:fidl_test_fuchsia_service_foo/fidl_async.dart';
import 'package:fuchsia_services/services.dart';
import 'package:test/test.dart';

const String serverUrl = '#meta/fuchsia-services-foo-test-server.cm';

void main() {
  test('launching and connecting to the foo service', () async {
    final incoming = Incoming.fromSvcPath();
    final fooProxy = FooProxy();
    incoming.connectToService(fooProxy);
    final response = await fooProxy.echo('foo');
    expect(response, 'foo');
  });
}
