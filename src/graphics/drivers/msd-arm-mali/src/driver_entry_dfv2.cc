// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpu.mali/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.gpu.mali/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fit/thread_safety.h>
#include <lib/magma/platform/platform_bus_mapper.h>
#include <lib/magma/platform/zircon/zircon_platform_logger_dfv2.h>
#include <lib/magma/platform/zircon/zircon_platform_status.h>
#include <lib/magma/util/short_macros.h>
#include <lib/magma_service/sys_driver/magma_driver_base.h>

#include "msd_arm_device.h"
#include "parent_device_dfv2.h"

#if MAGMA_TEST_DRIVER
constexpr char kDriverName[] = "mali-test";
using MagmaDriverBaseType = msd::MagmaTestDriverBase;

zx_status_t magma_indriver_test(ParentDevice* device);

#else
constexpr char kDriverName[] = "mali";
using MagmaDriverBaseType = msd::MagmaProductionDriverBase;
#endif

class MaliDriver : public MagmaDriverBaseType,
                   public fidl::WireServer<fuchsia_hardware_gpu_mali::MaliUtils> {
 public:
  MaliDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBaseType(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        mali_devfs_connector_(fit::bind_member<&MaliDriver::BindUtilsConnector>(this)) {}

  zx::result<> MagmaStart() override {
    zx::result info_resource = GetInfoResource();
    // Info resource may not be available on user builds.
    if (info_resource.is_ok()) {
      magma::PlatformBusMapper::SetInfoResource(std::move(*info_resource));
    }

    parent_device_ = ParentDeviceDFv2::Create(incoming(), take_config<config::Config>());
    if (!parent_device_) {
      MAGMA_LOG(ERROR, "Failed to create ParentDeviceDFv2");
      return zx::error(ZX_ERR_INTERNAL);
    }

    std::lock_guard lock(magma_mutex());

    set_magma_driver(msd::Driver::Create());
    if (!magma_driver()) {
      DMESSAGE("Failed to create MagmaDriver");
      return zx::error(ZX_ERR_INTERNAL);
    }

#if MAGMA_TEST_DRIVER
    set_unit_test_status(magma_indriver_test(parent_device_.get()));
#endif

    set_magma_system_device(msd::MagmaSystemDevice::Create(
        magma_driver(), magma_driver()->CreateDevice(parent_device_->ToDeviceHandle())));
    if (!magma_system_device()) {
      DMESSAGE("Failed to create MagmaSystemDevice");
      return zx::error(ZX_ERR_INTERNAL);
    }

    return zx::ok();
  }
  zx::result<> CreateAdditionalDevNodes() override {
    fidl::Arena arena;
    zx::result connector = mali_devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                     .connector(std::move(connector.value()))
                     .class_name("mali-util");

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, "magma_mali")
                    .devfs_args(devfs.Build())
                    .Build();

    auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
    zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed: %s", node_endpoints.status_string());

    fidl::WireResult result = node_client()->AddChild(args, std::move(controller_endpoints.server),
                                                      std::move(node_endpoints->server));
    mali_node_controller_.Bind(std::move(controller_endpoints.client));
    mali_node_.Bind(std::move(node_endpoints->client));
    return zx::ok();
  }

  void Stop() override {
    MagmaDriverBaseType::Stop();
    magma::PlatformBusMapper::SetInfoResource(zx::resource{});
  }

  void BindUtilsConnector(fidl::ServerEnd<fuchsia_hardware_gpu_mali::MaliUtils> server) {
    fidl::BindServer(dispatcher(), std::move(server), this);
  }

  void SetPowerState(fuchsia_hardware_gpu_mali::wire::MaliUtilsSetPowerStateRequest* request,
                     SetPowerStateCompleter::Sync& completer) override {
    std::lock_guard lock(magma_mutex());

    msd::Device* dev = magma_system_device()->msd_dev();

    static_cast<MsdArmDevice*>(dev)->SetPowerState(
        request->enabled,
        [completer = completer.ToAsync()](bool powered_on) mutable { completer.ReplySuccess(); });
  }

  void GetPowerGoals(GetPowerGoalsCompleter::Sync& completer) override {
    std::lock_guard lock(magma_mutex());

    msd::Device* dev = magma_system_device()->msd_dev();

    static_cast<MsdArmDevice*>(dev)->RunTaskOnDeviceThread(
        [completer = completer.ToAsync()](MsdArmDevice* dev) mutable -> magma::Status {
          auto power_goals = dev->GetPowerGoals();
          fidl::Arena arena;
          std::vector<fuchsia_gpu_magma::wire::PowerGoal> power_goal_fidl;
          if (power_goals.on_ready_for_work_token) {
            auto power_goal = fuchsia_gpu_magma::wire::PowerGoal::Builder(arena);
            power_goal.token(std::move(power_goals.on_ready_for_work_token));
            power_goal.type(fuchsia_gpu_magma::wire::PowerGoalType::kOnReadyForWork);
            power_goal_fidl.push_back(power_goal.Build());
          }

          completer.Reply(
              fidl::VectorView<fuchsia_gpu_magma::wire::PowerGoal>::FromExternal(power_goal_fidl));
          return MAGMA_STATUS_OK;
        });
  }

 private:
  std::unique_ptr<ParentDeviceDFv2> parent_device_;
  driver_devfs::Connector<fuchsia_hardware_gpu_mali::MaliUtils> mali_devfs_connector_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> mali_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> mali_node_controller_;
};

FUCHSIA_DRIVER_EXPORT(MaliDriver);
