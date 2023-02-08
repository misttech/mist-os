// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'package:logging/logging.dart';

import 'sl4f_client.dart';

final _log = Logger('flatland_example_sl4f');

class FlatlandExample {
  final Sl4f _sl4f;

  FlatlandExample(this._sl4f);

  Future<void> start() => _sl4f.request('flatland_example_facade.Start', null);

  Future<void> stop() => _sl4f.request('flatland_example_facade.Stop', null);
}
