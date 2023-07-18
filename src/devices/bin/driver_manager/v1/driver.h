// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_DRIVER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_DRIVER_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/fidl.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/binding.h>
#include <lib/fit/function.h>
#include <lib/zx/vmo.h>

#include <memory>
#include <variant>

#include <fbl/intrusive_double_list.h>
#include <fbl/string.h>
#include <fbl/unique_fd.h>

struct Driver {
  Driver() = default;

  // The name for this driver, parsed out of the driver's ELF header.
  fbl::String name;
  // The scheduler role to set for the default dispatcher created for the driver.
  // This may be an empty string.
  fbl::String default_dispatcher_scheduler_role;

  // The URL for this driver. It points to the driver's component manifest.
  fbl::String url;

  // If this is true, this driver should only be bound after /system/ comes up.
  bool fallback = false;

  // If this is true, the driver library has been instrumented with ASAN.
  bool is_asan = false;

  zx::vmo dso_vmo;

  // If this is valid, it's the root directory of the Driver's package.
  fbl::unique_fd package_dir;

  // A list of service uses parsed out of the driver's component manifest.
  std::vector<std::string> service_uses;
};

struct MatchedDriverInfo {
  // If this is true, the driver expects to be colcated with its parent.
  bool colocate = false;
  // If this is true, this driver expects to be loaded in DFv2.
  bool is_dfv2 = false;

  // If this is true, the driver should only be bound after /system/ comes up.
  bool is_fallback = false;

  // The type of package the driver is in.
  fuchsia_driver_index::DriverPackageType package_type;

  // The url for the driver component.
  std::string component_url;
};

using MatchedDriver =
    std::variant<MatchedDriverInfo, fuchsia_driver_index::MatchedCompositeNodeParentInfo>;

#define DRIVER_NAME_LEN_MAX 64

using DriverLoadCallback = fit::function<void(Driver* driver, const char* version)>;

zx_status_t load_driver(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args,
                        std::string_view url, zx::vmo driver_vmo,
                        const std::vector<std::string>& service_uses, DriverLoadCallback func);
zx::result<zx::vmo> load_manifest_vmo(
    const fidl::WireSyncClient<fuchsia_io::Directory>& package_dir, std::string_view resource_path);
zx::result<zx::vmo> load_driver_vmo(const fidl::WireSyncClient<fuchsia_io::Directory>& package_dir,
                                    const std::string& resource_path);

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_DRIVER_H_
