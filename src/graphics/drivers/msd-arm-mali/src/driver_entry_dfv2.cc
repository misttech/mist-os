// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpu.mali/cpp/driver/wire.h>
#include <fidl/fuchsia.hardware.gpu.mali/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
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

zx_status_t magma_indriver_test(ParentDevice* device);

#else
constexpr char kDriverName[] = "mali";
#endif

class MaliDriver : public msd::MagmaProductionDriverBase,
                   public fidl::WireServer<fuchsia_hardware_gpu_mali::MaliUtils> {
 public:
  MaliDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : msd::MagmaProductionDriverBase(kDriverName, std::move(start_args),
                                       std::move(driver_dispatcher)) {}

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
    {
      test_server_.set_unit_test_status(magma_indriver_test(parent_device_.get()));
      zx::result result = CreateTestService(test_server_);
      if (result.is_error()) {
        DMESSAGE("Failed to serve the TestService");
        return zx::error(ZX_ERR_INTERNAL);
      }
    }
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
    fuchsia_hardware_gpu_mali::UtilsService::InstanceHandler handler({
        .device =
            mali_utils_bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
    });

    return outgoing()->AddService<fuchsia_hardware_gpu_mali::UtilsService>(std::move(handler));
  }

  void Stop() override {
    msd::MagmaProductionDriverBase::Stop();
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

#if MAGMA_TEST_DRIVER
  msd::MagmaTestServer test_server_;
#endif

  fidl::ServerBindingGroup<fuchsia_hardware_gpu_mali::MaliUtils> mali_utils_bindings_;
};

FUCHSIA_DRIVER_EXPORT(MaliDriver);
