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
    const std::string& manifest_url) {
  zx::result url_result = GetPathFromUrl(manifest_url);
  if (url_result.is_error()) {
    LOGF(ERROR, "Failed to get path from '%s' %s", manifest_url.c_str(),
         url_result.status_string());
    return url_result.take_error();
  }
  std::string url = std::move(url_result.value());

  zx::result vmo_result = load_manifest_vmo(url);
  if (vmo_result.is_error()) {
    LOGF(ERROR, "Failed to load driver vmo: %s", vmo_result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }
  zx::vmo manifest_vmo = std::move(vmo_result.value());

  // Parse manifest for driver_url
  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo));
  if (manifest.is_error()) {
    LOGF(ERROR, "Failed to parse manifest: %s", manifest.status_string());
    return manifest.take_error();
  }

  zx::result base_path_result = GetBasePathFromUrl(manifest_url);
  if (base_path_result.is_error()) {
    LOGF(ERROR, "Failed to get base path from '%s' %s", manifest_url.c_str(),
         base_path_result.status_string());
    return base_path_result.take_error();
  }
  std::string base_path = std::move(base_path_result.value());

  zx::result resource_path = GetResourcePath(manifest->driver_url);
  if (resource_path.is_error()) {
    LOGF(ERROR, "Failed to get resource path from '%s' %s", manifest->driver_url.c_str(),
         resource_path.status_string());
    return resource_path.take_error();
  }

  url = base_path + "/" + resource_path.value();
  vmo_result = load_driver_vmo(url);
  if (vmo_result.is_error()) {
    LOGF(ERROR, "Failed to load driver vmo: %s", zx_status_get_string(vmo_result.error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  zx::vmo driver_vmo = std::move(vmo_result.value());

  Driver* driver = nullptr;
  DriverLoadCallback callback = [&driver](Driver* d, const char* version) mutable { driver = d; };
  if (zx_status_t status = load_driver(boot_args_, manifest_url, std::move(driver_vmo),
                                       std::move(manifest->service_uses), std::move(callback));
      status != ZX_OK) {
    LOGF(ERROR, "Failed to load driver: %s", zx_status_get_string(status));
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (!driver) {
    LOGF(INFO, "Driver %s not found, probably disabled", manifest_url.c_str());
    return zx::ok(nullptr);
  }

  int fd;
  if ((fd = open(base_path.c_str(), O_RDONLY, O_DIRECTORY)) < 0) {
    LOGF(ERROR, "Failed to open package dir: '%s'", base_path_result.value().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }
  driver->package_dir = fbl::unique_fd(fd);

  return zx::ok(std::unique_ptr<Driver>(driver));
}

}  // namespace internal
