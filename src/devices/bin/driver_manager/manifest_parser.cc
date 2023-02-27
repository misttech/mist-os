// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/manifest_parser.h"

#include <lib/zx/result.h>

#include "src/lib/fxl/strings/substitute.h"
#include "src/lib/pkg_url/fuchsia_pkg_url.h"

namespace {

const std::string kFuchsiaPkgPrefix = "fuchsia-pkg://";
const std::string kFuchsiaBootPrefix = "fuchsia-boot://";
const std::string kRelativeUrlPrefix = "#";

bool IsFuchsiaPkgScheme(std::string_view url) {
  return url.compare(0, kFuchsiaPkgPrefix.length(), kFuchsiaPkgPrefix) == 0;
}

zx::result<std::string> GetResourcePath(std::string_view url) {
  size_t seperator = url.find('#');
  if (seperator == std::string::npos) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return zx::ok(url.substr(seperator + 1));
}

bool IsRelativeUrl(std::string_view url) {
  return url.compare(0, kRelativeUrlPrefix.length(), kRelativeUrlPrefix) == 0;
}

}  // namespace

bool IsFuchsiaBootScheme(std::string_view url) {
  return url.compare(0, kFuchsiaBootPrefix.length(), kFuchsiaBootPrefix) == 0;
}

zx::result<std::string> GetBasePathFromUrl(const std::string& url) {
  if (IsFuchsiaPkgScheme(url)) {
    component::FuchsiaPkgUrl package_url;
    if (!package_url.Parse(url)) {
      LOGF(ERROR, "Failed to parse fuchsia url: %s", url.c_str());
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok(fxl::Substitute("/pkgfs/packages/$0/$1", package_url.package_name(),
                                  package_url.variant()));
  }
  if (IsFuchsiaBootScheme(url)) {
    auto resource_path = GetResourcePath(url);
    if (resource_path.is_error()) {
      LOGF(ERROR, "Failed to parse boot url: %s", url.c_str());
      return resource_path;
    }
    return zx::ok("/boot");
  }
  if (IsRelativeUrl(url)) {
    auto resource_path = GetResourcePath(url);
    if (resource_path.is_error()) {
      LOGF(ERROR, "Failed to parse test url: %s", url.c_str());
      return resource_path;
    }
    return zx::ok("/pkg");
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<std::string> GetPathFromUrl(const std::string& url) {
  if (IsFuchsiaPkgScheme(url)) {
    component::FuchsiaPkgUrl package_url;
    if (!package_url.Parse(url)) {
      LOGF(ERROR, "Failed to parse fuchsia url: %s", url.c_str());
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok(fxl::Substitute("/pkgfs/packages/$0/$1/$2", package_url.package_name(),
                                  package_url.variant(), package_url.resource_path()));
  }
  if (IsFuchsiaBootScheme(url)) {
    auto resource_path = GetResourcePath(url);
    if (resource_path.is_error()) {
      LOGF(ERROR, "Failed to parse boot url: %s", url.c_str());
      return resource_path;
    }
    return zx::ok("/boot/" + resource_path.value());
  }
  if (IsRelativeUrl(url)) {
    auto resource_path = GetResourcePath(url);
    if (resource_path.is_error()) {
      LOGF(ERROR, "Failed to parse test url: %s", url.c_str());
      return resource_path;
    }
    return zx::ok("/pkg/" + resource_path.value());
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}
