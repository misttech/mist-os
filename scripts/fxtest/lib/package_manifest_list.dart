// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:io';

import 'package:fxtest/fxtest.dart';
import 'package:meta/meta.dart';

class PackageManifestList {
  final Set<String> manifests;

  PackageManifestList({
    this.manifests = const {},
  });

  /// Constructs a [PackageRepository] from the contents of a
  /// package-repositories.json manifest file.
  @visibleForTesting
  factory PackageManifestList.fromJson(Map<String, dynamic> json) {
    String version = json['version'];
    if (version != '1') {
      throw PackageManifestListParseException("unknown version ${version}");
    }

    Set<String> manifests = Set<String>.from(json['content']);

    return PackageManifestList(
      manifests: manifests,
    );
  }
}
