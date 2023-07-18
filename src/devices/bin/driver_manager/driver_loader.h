// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_LOADER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_LOADER_H_

#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/fidl.h>
#include <threads.h>

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_manager/base_package_resolver.h"
#include "src/devices/bin/driver_manager/composite_node_spec/composite_manager_bridge.h"
#include "src/devices/bin/driver_manager/device.h"
#include "src/devices/bin/driver_manager/driver.h"

class Coordinator;

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace fdi = fuchsia_driver_index;

class DriverLoader {
 public:
  // Takes in an unowned connection to boot arguments. boot_args must outlive DriverLoader.
  // Takes in an unowned connection to base_resolver. base_resolver must outlive DriverLoader.
  explicit DriverLoader(fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args,
                        fidl::WireSharedClient<fdi::DriverIndex> driver_index,
                        internal::PackageResolverInterface* base_resolver,
                        async_dispatcher_t* dispatcher,
                        bool delay_fallback_until_base_drivers_indexed,
                        internal::PackageResolverInterface* universe_resolver)
      : base_resolver_(base_resolver),
        driver_index_(std::move(driver_index)),
        include_fallback_drivers_(!delay_fallback_until_base_drivers_indexed),
        universe_resolver_(universe_resolver) {}

  // This will schedule a task on the async_dispatcher that will return
  // when DriverIndex has loaded the base drivers. When the task completes,
  // `callback` will be called.
  void WaitForBaseDrivers(fit::callback<void()> callback);

  struct MatchDeviceConfig {
    // If this is non-empty, then only drivers who match this url suffix will be matched.
    std::string_view driver_url_suffix;
    // This config should only be true after the base drivers are loaded.
    // We will need to go through all the devices and bind just base drivers
    // and fallback drivers.
    bool only_return_base_and_fallback_drivers = false;
  };

  void AddCompositeNodeSpec(fuchsia_driver_framework::wire::CompositeNodeSpec spec,
                            AddToIndexCallback callback);

  const std::vector<MatchedDriver> MatchDeviceDriverIndex(const fbl::RefPtr<Device>& dev,
                                                          const MatchDeviceConfig& config);
  const std::vector<MatchedDriver> MatchDeviceDriverIndex(
      std::string_view name, const fbl::Array<const zx_device_prop_t>& props,
      const fbl::Array<const StrProperty>& str_props, uint32_t protocol_id,
      const MatchDeviceConfig& config);

  const std::vector<MatchedDriver> MatchPropertiesDriverIndex(
      std::string_view name, fidl::VectorView<fdf::wire::NodeProperty> props,
      const MatchDeviceConfig& config);

  const Driver* LoadDriverUrl(const std::string& manifest_url, bool use_universe_resolver = false);

 private:
  const Driver* UrlToDriver(const std::string& url);

  // Drivers we cached from the DriverIndex, mapped via URL.
  std::unordered_map<std::string, std::unique_ptr<Driver>> driver_index_drivers_;

  internal::PackageResolverInterface* base_resolver_;
  fidl::WireSharedClient<fdi::DriverIndex> driver_index_;

  // When this is true we will return DriverIndex fallback drivers.
  // This is true after the system is loaded (or if require_system is false)
  bool include_fallback_drivers_;

  // The universe package resolver.
  // Currently used only for ephemeral drivers.
  internal::PackageResolverInterface* universe_resolver_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_LOADER_H_
