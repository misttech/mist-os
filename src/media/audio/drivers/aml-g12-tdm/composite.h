// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_H_

#include <fidl/fuchsia.hardware.audio/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <algorithm>
#include <memory>

#include "src/media/audio/drivers/aml-g12-tdm/composite-server.h"

namespace audio::aml_g12 {

constexpr char kDriverName[] = "aml-g12-audio-composite";

struct PowerConfiguration {
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control_client;
  fidl::ClientEnd<fuchsia_power_broker::Lessor> lessor_client;
  fidl::ClientEnd<fuchsia_power_broker::CurrentLevel> current_level_client;
  fidl::ClientEnd<fuchsia_power_broker::RequiredLevel> required_level_client;
};

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&Driver::Serve>(this)) {}

  ~Driver() override = default;

  zx::result<> Start() override;

 private:
  zx::result<> CreateDevfsNode();
  void Serve(fidl::ServerEnd<fuchsia_hardware_audio::Composite> server) {
    bindings_.AddBinding(dispatcher(), std::move(server), server_.get(),
                         fidl::kIgnoreBindingClosure);
  }
  zx::result<PowerConfiguration> GetPowerConfiguration(
      const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev);

  std::unique_ptr<AudioCompositeServer> server_;
  fidl::ServerBindingGroup<fuchsia_hardware_audio::Composite> bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev_;
  driver_devfs::Connector<fuchsia_hardware_audio::Composite> devfs_connector_;
};

}  // namespace audio::aml_g12

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_H_
