// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_MANIFEST_PARSER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_MANIFEST_PARSER_H_

#include <lib/zx/result.h>
#include <lib/zx/vmo.h>

#include <string>
#include <vector>

#include "src/devices/lib/log/log.h"

// Get the package relative path after the `#` character.
zx::result<std::string> GetResourcePath(std::string_view url);

// Get the path in the namespace to the base directory of a package for
// fuchsia-boot:// and relative package URLs.
zx::result<std::string> GetBasePathFromUrl(const std::string& url);

// Returns true if url starts with 'fuchsia-boot://'
bool IsFuchsiaBootScheme(std::string_view url);

struct ManifestContent {
  std::string driver_url;
  std::vector<std::string> service_uses;
  std::string default_dispatcher_scheduler_role;
};

// Parse out important fields from the component manifest.
zx::result<ManifestContent> ParseComponentManifest(zx::vmo vmo);

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_MANIFEST_PARSER_H_
