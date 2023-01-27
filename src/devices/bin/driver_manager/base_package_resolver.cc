// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

#include "src/devices/bin/driver_manager/base_package_resolver.h"

#include <fcntl.h>

#include "src/devices/bin/driver_manager/manifest_parser.h"
#include "src/devices/lib/log/log.h"
#include "src/zircon/lib/zircon/include/zircon/status.h"

namespace internal {

zx::result<std::unique_ptr<Driver>> BasePackageResolver::FetchDriver(
    const std::string& package_url) {
  zx::result url_result = GetPathFromUrl(package_url);
  if (url_result.is_error()) {
    LOGF(ERROR, "Failed to get path from '%s' %s", package_url.c_str(), url_result.status_string());
    return url_result.take_error();
  }
  std::string url = std::move(url_result.value());

  zx::result vmo_result = load_vmo(url);
  if (vmo_result.is_error()) {
    LOGF(ERROR, "Failed to load driver vmo: %s", zx_status_get_string(vmo_result.error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  zx::vmo vmo = std::move(vmo_result.value());

  Driver* driver = nullptr;
  DriverLoadCallback callback = [&driver](Driver* d, const char* version) mutable { driver = d; };
  if (zx_status_t status =
          load_driver_vmo(boot_args_, package_url, std::move(vmo), std::move(callback));
      status != ZX_OK) {
    LOGF(ERROR, "Failed to load driver: %s", zx_status_get_string(status));
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (!driver) {
    LOGF(INFO, "Driver %s not found, probably disabled", package_url.data());
    return zx::ok(nullptr);
  }

  zx::result base_path_result = GetBasePathFromUrl(package_url);
  if (base_path_result.is_error()) {
    return base_path_result.take_error();
  }
  std::string base_path = std::move(base_path_result.value());

  int fd;
  if ((fd = open(base_path.c_str(), O_RDONLY, O_DIRECTORY)) < 0) {
    LOGF(ERROR, "Failed to open package dir: '%s'", base_path_result.value().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }
  driver->package_dir = fbl::unique_fd(fd);

  return zx::ok(std::unique_ptr<Driver>(driver));
}

}  // namespace internal
