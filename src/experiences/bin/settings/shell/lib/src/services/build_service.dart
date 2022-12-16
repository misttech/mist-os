// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';
import 'dart:ui';

import 'package:fidl_fuchsia_buildinfo/fidl_async.dart' as buildinfo;
import 'package:fuchsia_services/services.dart';
import 'package:shell_settings/src/services/task_service.dart';

/// Defines a [TaskService] to watch for build version of system.
class BuildService implements TaskService {
  late final VoidCallback onChanged;

  buildinfo.ProviderProxy? _provider;
  String _buildVersion = '--';

  @override
  Future<void> start() async {
    _provider = buildinfo.ProviderProxy();
    Incoming.fromSvcPath().connectToService(_provider);

    if (_provider != null) {
      // Get the build info.
      _provider!.getBuildInfo().then((buildInfo) {
        buildVersion = buildInfo.version ?? '--';
      });
    }
  }

  @override
  Future<void> stop() async {
    dispose();
  }

  @override
  void dispose() {
    _provider?.ctrl.close();
  }

  String get buildVersion => _buildVersion;
  set buildVersion(String buildVersion) {
    _buildVersion = buildVersion;
    onChanged();
  }
}
