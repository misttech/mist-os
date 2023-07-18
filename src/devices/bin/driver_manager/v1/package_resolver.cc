// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/package_resolver.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.pkg/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>

#include <fbl/string_printf.h>

#include "src/devices/bin/driver_manager/v1/manifest_parser.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/storage/vfs/cpp/vfs.h"

namespace fio = fuchsia_io;

namespace internal {

zx::result<std::unique_ptr<Driver>> PackageResolver::FetchDriver(const std::string& manifest_url) {
  component::FuchsiaPkgUrl parsed_url;
  if (!parsed_url.Parse(std::string(manifest_url))) {
    LOGF(ERROR, "Failed to parse manifest url: %s", manifest_url.data());
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result package_dir_result = Resolve(parsed_url.ToString());
  if (!package_dir_result.is_ok()) {
    LOGF(ERROR, "Failed to resolve package url %s, err %d", parsed_url.ToString().c_str(),
         package_dir_result.status_value());
    return package_dir_result.take_error();
  }

  zx::result manifest_vmo_result =
      load_manifest_vmo(package_dir_result.value(), parsed_url.resource_path());
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

  if (!parsed_url.Parse(std::string(manifest->driver_url))) {
    LOGF(ERROR, "Failed to parse package url: %s", manifest->driver_url.data());
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result driver_vmo_result =
      load_driver_vmo(package_dir_result.value(), parsed_url.resource_path());
  if (driver_vmo_result.status_value()) {
    return driver_vmo_result.take_error();
  }

  Driver* driver = nullptr;
  DriverLoadCallback callback = [&driver](Driver* d, const char* version) mutable { driver = d; };

  zx_status_t status = load_driver(boot_args_, std::string_view(parsed_url.ToString()),
                                   std::move(driver_vmo_result.value()),
                                   std::move(manifest->service_uses), std::move(callback));
  if (status != ZX_OK) {
    return zx::error(status);
  }
  if (!driver) {
    LOGF(INFO, "Driver %s not found, probably disabled", manifest_url.c_str());
    return zx::ok(nullptr);
  }

  fbl::unique_fd package_dir_fd;
  status = fdio_fd_create(package_dir_result.value().TakeClientEnd().TakeChannel().release(),
                          package_dir_fd.reset_and_get_address());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to create package_dir_fd: %sd", zx_status_get_string(status));
    return zx::error(status);
  }
  driver->package_dir = std::move(package_dir_fd);
  driver->default_dispatcher_scheduler_role = manifest->default_dispatcher_scheduler_role;
  return zx::ok(std::unique_ptr<Driver>(driver));
}

zx_status_t PackageResolver::ConnectToResolverService() {
  zx::result client_end = component::Connect<fuchsia_component_resolution::Resolver>(
      "/svc/fuchsia.component.resolution.Resolver-full");
  if (client_end.is_error()) {
    return client_end.error_value();
  }
  resolver_client_ = fidl::WireSyncClient(std::move(*client_end));
  return ZX_OK;
}

zx::result<fidl::WireSyncClient<fio::Directory>> PackageResolver::Resolve(
    const std::string& component_url) {
  if (!resolver_client_.is_valid()) {
    zx_status_t status = ConnectToResolverService();
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to connect to package resolver service");
      return zx::error(status);
    }
  }

  // TODO(fxbug.dev/123042) This is synchronous for now so we can get the proof of concept working.
  // Eventually we will want to do this asynchronously.
  fidl::WireResult result =
      resolver_client_->Resolve(::fidl::StringView(fidl::StringView::FromExternal(component_url)));
  if (!result.ok() || result->is_error()) {
    LOGF(ERROR, "Failed to resolve package");
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

zx::result<zx::vmo> PackageResolver::LoadDriver(
    const fidl::WireSyncClient<fuchsia_io::Directory>& package_dir,
    const component::FuchsiaPkgUrl& package_url) {
  const fio::wire::OpenFlags kFileRights =
      fio::wire::OpenFlags::kRightReadable | fio::wire::OpenFlags::kRightExecutable;
  const fio::wire::VmoFlags kDriverVmoFlags =
      fio::wire::VmoFlags::kRead | fio::wire::VmoFlags::kExecute;

  // Open and duplicate the driver vmo.
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::File>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  fidl::OneWayStatus file_open_result = package_dir->Open(
      kFileRights, {} /* mode */, fidl::StringView::FromExternal(package_url.resource_path()),
      fidl::ServerEnd<fuchsia_io::Node>(endpoints->server.TakeChannel()));
  if (!file_open_result.ok()) {
    LOGF(ERROR, "Failed to open driver file: %s", package_url.resource_path().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }

  fidl::WireSyncClient file_client{std::move(endpoints->client)};
  fidl::WireResult file_res = file_client->GetBackingMemory(kDriverVmoFlags);
  if (!file_res.ok()) {
    LOGF(ERROR, "Failed to get driver vmo: %s", file_res.FormatDescription().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (file_res->is_error()) {
    LOGF(ERROR, "Failed to get driver vmo: %s", zx_status_get_string(file_res->error_value()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(std::move(file_res->value()->vmo));
}

}  // namespace internal
