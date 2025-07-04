// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_ROOT_DEVICE_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_ROOT_DEVICE_H_

#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/zx/result.h>

#include "sdio-controller-device.h"
#include "sdmmc-block-device.h"
#include "src/devices/block/drivers/sdmmc/sdmmc_config.h"

namespace sdmmc {

constexpr uint32_t kInitializationFrequencyHz = 400'000;

class SdmmcDevice;

class SdmmcRootDevice : public fdf::DriverBase {
 public:
  SdmmcRootDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("sdmmc", std::move(start_args), std::move(dispatcher)),
        config_(take_config<sdmmc_config::Config>()) {}

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Called by children (or grandchildren) of this device for invoking AddChild() or instantiating
  // compat::DeviceServer.
  fidl::WireSyncClient<fuchsia_driver_framework::Node>& root_node() { return root_node_; }
  std::string_view driver_name() const { return name(); }
  const std::shared_ptr<fdf::Namespace>& driver_incoming() const { return incoming(); }
  std::shared_ptr<fdf::OutgoingDirectory>& driver_outgoing() { return outgoing(); }
  async_dispatcher_t* driver_async_dispatcher() const { return dispatcher(); }
  const fdf::UnownedSynchronizedDispatcher& driver_dispatcher() const {
    return fdf::DriverBase::driver_dispatcher();
  }
  const std::optional<std::string>& driver_node_name() const { return node_name(); }
  inspect::ComponentInspector& driver_inspector() { return inspector(); }
  const sdmmc_config::Config& config() const { return config_; }

  // Visible for testing.
  const std::variant<std::monostate, std::unique_ptr<SdioControllerDevice>,
                     std::unique_ptr<SdmmcBlockDevice>>&
  child_device() const {
    return child_device_;
  }

 protected:
  virtual zx_status_t Init(const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata);

  std::variant<std::monostate, std::unique_ptr<SdioControllerDevice>,
               std::unique_ptr<SdmmcBlockDevice>>
      child_device_;

 private:
  // Returns the SDMMC metadata with default values for any fields that are not present (or if the
  // metadata itself is not present). Returns an error if the metadata could not be decoded.
  zx::result<fuchsia_hardware_sdmmc::SdmmcMetadata> GetMetadata();

  template <class DeviceType>
  zx::result<std::unique_ptr<SdmmcDevice>> MaybeAddDevice(
      const std::string& name, std::unique_ptr<SdmmcDevice> sdmmc,
      const fuchsia_hardware_sdmmc::SdmmcMetadata& metadata);

  sdmmc_config::Config config_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> root_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDMMC_SDMMC_ROOT_DEVICE_H_
