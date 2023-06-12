// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'package:fxtest/fxtest.dart';
import 'package:test/test.dart';

void main() {
  group('reads package manifest list information correctly', () {
    test('parsing all_package_manifests.list', () {
      Map<String, dynamic> manifestListJson = {
        'version': '1',
        'content': {
          'manifests': [
            'foo.package_manifest.json',
            'bar.package_manifest.json',
          ]
        }
      };
      PackageManifestList manifestList =
          PackageManifestList.fromJson(manifestListJson);

      expect(manifestList.manifests, [
        'foo.package_manifest.json',
        'bar.package_manifest.json',
      ]);
    });
  });
}
