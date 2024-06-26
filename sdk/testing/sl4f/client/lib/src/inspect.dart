// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// @dart=2.12

import 'dart:async';

import 'package:logging/logging.dart';

import 'sl4f_client.dart';

/// Read inspect data from components running on the device.
///
/// Inspect (https://fuchsia.dev/fuchsia-src/development/inspect) gives you
/// access to current state information exposed by components.
class Inspect {
  final Sl4f sl4f;

  /// Construct an [Inspect] object.
  Inspect(this.sl4f);

  /// Gets the inspect data filtering using the given [selectors].
  ///
  /// The result is a list of deserialized JSON Maps with key value pairs for
  /// the matching components and nodes.
  ///
  /// A selector consists of the realm path, component name and a path to a node
  /// or property.
  /// It accepts wildcards.
  /// For example:
  ///   a/*/test:path/to/*/node:prop
  ///   a/*/test:root
  ///
  /// See: https://fuchsia.googlesource.com/fuchsia/+/HEAD/sdk/fidl/fuchsia.diagnostics/selector.fidl
  ///
  /// Returns an empty list if nothing is found.
  Future<List<Map<String, dynamic>>> snapshot(List<String> selectors) async {
    final hierarchyList =
        await sl4f.request('diagnostics_facade.SnapshotInspect', {
              'selectors': selectors,
              'service_name': 'fuchsia.diagnostics.ArchiveAccessor',
            }) ??
            [];
    return hierarchyList.cast<Map<String, dynamic>>();
  }

  /// Gets the inspect data for all components currently running in the system.
  ///
  /// Returns an empty list if nothing is found.
  Future<List<Map<String, dynamic>>> snapshotAll() async {
    return await snapshot([]);
  }

  /// Gets the payload of the first found hierarchy matching the given selectors
  /// under root.
  ///
  /// Returns null if no hierarchy was found.
  Future<Map<String, dynamic>?> snapshotRoot(
    String componentSelector,
  ) async {
    final hierarchies = await snapshot(
      ['$componentSelector:root'],
    );
    if (hierarchies.isEmpty) {
      return null;
    }
    final resultHierarchy = hierarchies.first;
    if (resultHierarchy['payload'] == null) {
      _log.warning('Got null payload for "$componentSelector:root". '
          'Metadata: ${resultHierarchy['metadata']}');
      return null;
    }
    return resultHierarchy['payload']['root'];
  }
}

final _log = Logger('Inspect');
