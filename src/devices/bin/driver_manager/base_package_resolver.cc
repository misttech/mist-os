// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

#include "src/devices/bin/driver_manager/base_package_resolver.h"

#include <fcntl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/fd.h>

#include "src/devices/bin/driver_manager/manifest_parser.h"
#include "src/devices/lib/log/log.h"
#include "src/zircon/lib/zircon/include/zircon/status.h"

namespace fio = fuchsia_io;

namespace internal {

zx::result<std::unique_ptr<Driver>> BasePackageResolver::FetchDriver(
    const std::string& manifest_url) {
  zx::result package_dir_result = GetPackageDir(manifest_url);
  if (package_dir_result.is_error()) {
    LOGF(ERROR, "Failed to get package dir for url '%s' %s", manifest_url.c_str(),
         package_dir_result.status_string());
    return package_dir_result.take_error();
  }
  fidl::WireSyncClient<fio::Directory> package_dir = std::move(package_dir_result.value());

  zx::result manifest_resource_path_result = GetResourcePath(manifest_url);
  if (manifest_resource_path_result.is_error()) {
    LOGF(ERROR, "Failed to get resource path for url: '%s' %s", manifest_url.c_str(),
         manifest_resource_path_result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }
  std::string manifest_resource_path = manifest_resource_path_result.value();

  zx::result manifest_vmo_result = load_manifest_vmo(package_dir, manifest_resource_path);
  if (manifest_vmo_result.is_error()) {
    LOGF(ERROR, "Failed to load manifest vmo: %s", manifest_vmo_result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Parse manifest for driver_url
  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo_result.value()));
  if (manifest.is_error()) {
    LOGF(ERROR, "Failed to parse manifest: %s", manifest.status_string());
    return manifest.take_error();
  }

  zx::result driver_resource_path_result = GetResourcePath(manifest->driver_url);
  if (driver_resource_path_result.is_error()) {
    LOGF(ERROR, "Failed to get resource path for url: '%s' %s", manifest->driver_url.c_str(),
         driver_resource_path_result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }
  std::string driver_resource_path = driver_resource_path_result.value();

  zx::result driver_vmo_result = load_driver_vmo(package_dir, driver_resource_path);
  if (driver_vmo_result.status_value()) {
    return driver_vmo_result.take_error();
  }
  zx::vmo driver_vmo = std::move(driver_vmo_result.value());

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

  fbl::unique_fd package_dir_fd;
  if (zx_status_t status = fdio_fd_create(package_dir.TakeClientEnd().TakeChannel().release(),
                                          package_dir_fd.reset_and_get_address());
      status != ZX_OK) {
    LOGF(ERROR, "Failed to create package_dir_fd: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  driver->package_dir = std::move(package_dir_fd);
  driver->default_dispatcher_scheduler_role = manifest->default_dispatcher_scheduler_role;
  return zx::ok(std::unique_ptr<Driver>(driver));
}

zx::result<fidl::WireSyncClient<fio::Directory>> BasePackageResolver::GetPackageDir(
    const std::string& url) {
  if (component::FuchsiaPkgUrl::IsFuchsiaPkgScheme(url)) {
    return Resolve(url);
  }

  zx::result base_path_result = GetBasePathFromUrl(url);
  if (base_path_result.is_error()) {
    LOGF(ERROR, "Failed to get base path of url: '%s'", url.c_str());
    return base_path_result.take_error();
  }
  std::string base_path = std::move(base_path_result.value());

  int fd;
  if ((fd = open(base_path.c_str(), O_RDONLY, O_DIRECTORY)) < 0) {
    LOGF(ERROR, "Failed to open package dir: '%s'", base_path.c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }

  fidl::ClientEnd<fio::Directory> client_end;
  if (auto status = fdio_fd_transfer(fd, client_end.channel().reset_and_get_address());
      status != ZX_OK) {
    LOGF(ERROR, "Failed to transfer fd from: '%s'", base_path.c_str());
    return zx::error_result(status);
  }
  return zx::ok(fidl::WireSyncClient<fio::Directory>{std::move(client_end)});
}

zx::result<fidl::WireSyncClient<fio::Directory>> BasePackageResolver::Resolve(
    const std::string& component_url) {
  if (!resolver_client_.is_valid()) {
    if (zx_status_t status = ConnectToResolverService(); status != ZX_OK) {
      LOGF(ERROR, "Failed to connect to base package resolver service %s",
           zx_status_get_string(status));
      return zx::error(status);
    }
  }

  // TODO(fxbug.dev/123042) This is synchronous for now so we can get the proof of concept working.
  // Eventually we will want to do this asynchronously.
  auto result =
      resolver_client_->Resolve(fidl::StringView(fidl::StringView::FromExternal(component_url)));
  if (!result.ok() || result->is_error()) {
    LOGF(ERROR, "Failed to resolve base package");
    if (!result.ok()) {
      return zx::error(ZX_ERR_INTERNAL);
    } else {
      switch (result->error_value()) {
        case fuchsia_component_resolution::wire::ResolverError::kIo:
          return zx::error(ZX_ERR_IO);
        case fuchsia_component_resolution::wire::ResolverError::kManifestNotFound:
          return zx::error(ZX_ERR_NOT_FOUND);
        case fuchsia_component_resolution::wire::ResolverError::kPackageNotFound:
          return zx::error(ZX_ERR_NOT_FOUND);
        case fuchsia_component_resolution::wire::ResolverError::kResourceUnavailable:
          return zx::error(ZX_ERR_UNAVAILABLE);
        case fuchsia_component_resolution::wire::ResolverError::kInvalidManifest:
          return zx::error(ZX_ERR_INVALID_ARGS);
        case fuchsia_component_resolution::wire::ResolverError::kInvalidArgs:
          return zx::error(ZX_ERR_INVALID_ARGS);
        case fuchsia_component_resolution::wire::ResolverError::kInvalidAbiRevision:
          return zx::error(ZX_ERR_INVALID_ARGS);
        case fuchsia_component_resolution::wire::ResolverError::kNoSpace:
          return zx::error(ZX_ERR_NO_SPACE);
        case fuchsia_component_resolution::wire::ResolverError::kNotSupported:
          return zx::error(ZX_ERR_NOT_SUPPORTED);
        case fuchsia_component_resolution::wire::ResolverError::kAbiRevisionNotFound:
          return zx::error(ZX_ERR_NOT_FOUND);
        default:
          return zx::error(ZX_ERR_INTERNAL);
      }
    }
  }
  return zx::ok(fidl::WireSyncClient(std::move(result.value()->component.package().directory())));
}

zx_status_t BasePackageResolver::ConnectToResolverService() {
  auto client_end = component::Connect<fuchsia_component_resolution::Resolver>(
      "/svc/fuchsia.component.resolution.Resolver-base");
  if (client_end.is_error()) {
    return client_end.error_value();
  }
  resolver_client_ = fidl::WireSyncClient(std::move(*client_end));
  return ZX_OK;
}

}  // namespace internal
