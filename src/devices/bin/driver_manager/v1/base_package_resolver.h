// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_BASE_PACKAGE_RESOLVER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_BASE_PACKAGE_RESOLVER_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.pkg/cpp/wire.h>

#include "src/devices/bin/driver_manager/v1/package_resolver.h"
#include "zircon/errors.h"

namespace internal {
class BasePackageResolver : public PackageResolverInterface {
 public:
  // Takes in an unowned connection to boot arguments. boot_args must outlive BasePackageResolver.
  explicit BasePackageResolver(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args)
      : boot_args_(boot_args) {}

  zx::result<std::unique_ptr<Driver>> FetchDriver(const std::string& manifest_url) override;

 private:
  // Creates the directory client for |url|.
  zx::result<fidl::WireSyncClient<fuchsia_io::Directory>> GetPackageDir(const std::string& url);

  // Connects to the base package resolver service if not already connected.
  zx_status_t ConnectToResolverService();

  // Returns a resolved component for a fuchsia-pkg:// |component_url|.
  zx::result<fidl::WireSyncClient<fuchsia_io::Directory>> Resolve(const std::string& component_url);

  fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args_;
  fidl::WireSyncClient<fuchsia_component_resolution::Resolver> resolver_client_;
};
}  // namespace internal

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_BASE_PACKAGE_RESOLVER_H_
