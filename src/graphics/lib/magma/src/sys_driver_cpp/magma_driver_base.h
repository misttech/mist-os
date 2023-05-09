// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_CPP_MAGMA_DRIVER_BASE_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_CPP_MAGMA_DRIVER_BASE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/lifecycle.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fit/thread_safety.h>

#include "magma_util/macros.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_logger_dfv2.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_status.h"
#include "sys_driver_cpp/magma_driver.h"

template <typename FidlDeviceType>
class MagmaDriverBase : public fdf::DriverBase, public fidl::WireServer<FidlDeviceType> {
 public:
  using fws = fidl::WireServer<FidlDeviceType>;

  MagmaDriverBase(std::string_view name, fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(name, std::move(start_args), std::move(driver_dispatcher)),
        magma_devfs_connector_(fit::bind_member<&MagmaDriverBase::BindConnector>(this)) {}

  zx::result<> Start() override {
    teardown_logger_callback_ =
        magma::InitializePlatformLoggerForDFv2(&logger(), std::string(name()));

    if (zx::result result = MagmaStart(); result.is_error()) {
      node().reset();
      return result.take_error();
    }

    node_client_.Bind(std::move(node()));

    auto defer_teardown = fit::defer([this]() { node_client_ = {}; });

    if (zx::result result = CreateDevfsNode(); result.is_error()) {
      return result.take_error();
    }
    MAGMA_LOG(INFO, "MagmaDriverBase::Start completed for MSD %s", std::string(name()).c_str());
    defer_teardown.cancel();
    return zx::ok();
  }

  void Stop() override {
    std::lock_guard lock(magma_mutex_);
    if (magma_system_device_) {
      magma_system_device_->Shutdown();
    }
    magma_system_device_.reset();
    magma_driver_.reset();
    teardown_logger_callback_.call();
  }

  // Initialize MagmaDriver and MagmaSystemDevice.
  virtual zx::result<> MagmaStart() = 0;

  std::mutex& magma_mutex() FIT_RETURN_CAPABILITY(magma_mutex_) { return magma_mutex_; }

  MagmaDriver* magma_driver() FIT_REQUIRES(magma_mutex_) { return magma_driver_.get(); }

  void set_magma_driver(std::unique_ptr<MagmaDriver> magma_driver) FIT_REQUIRES(magma_mutex_) {
    ZX_DEBUG_ASSERT(!magma_driver_);
    magma_driver_ = std::move(magma_driver);
  }

  void set_magma_system_device(std::shared_ptr<MagmaSystemDevice> magma_system_device)
      FIT_REQUIRES(magma_mutex_) {
    ZX_DEBUG_ASSERT(!magma_system_device_);
    magma_system_device_ = std::move(magma_system_device);
  }

  MagmaSystemDevice* magma_system_device() FIT_REQUIRES(magma_mutex_) {
    return magma_system_device_.get();
  }

  template <typename T>
  bool CheckSystemDevice(T& completer) FIT_REQUIRES(magma_mutex_) {
    if (!magma_system_device_) {
      MAGMA_LOG(WARNING, "Got message on torn-down device");
      completer.Close(ZX_ERR_BAD_STATE);
      return false;
    }
    return true;
  }

  void Query(typename fws::QueryRequestView request,
             typename fws::QueryCompleter::Sync& _completer) override {
    MAGMA_DLOG("MagmaDriverBase::Query");
    std::lock_guard lock(magma_mutex_);
    if (!CheckSystemDevice(_completer))
      return;

    zx_handle_t result_buffer = ZX_HANDLE_INVALID;
    uint64_t result = 0;

    magma::Status status =
        magma_system_device_->Query(fidl::ToUnderlying(request->query_id), &result_buffer, &result);
    if (!status.ok()) {
      _completer.ReplyError(magma::ToZxStatus(status.get()));
      return;
    }

    if (result_buffer != ZX_HANDLE_INVALID) {
      _completer.ReplySuccess(
          fuchsia_gpu_magma::wire::DeviceQueryResponse::WithBufferResult(zx::vmo(result_buffer)));
    } else {
      _completer.ReplySuccess(fuchsia_gpu_magma::wire::DeviceQueryResponse::WithSimpleResult(
          fidl::ObjectView<uint64_t>::FromExternal(&result)));
    }
  }

  void Connect2(typename fws::Connect2RequestView request,
                typename fws::Connect2Completer::Sync& _completer) override {
    MAGMA_DLOG("MagmaDriverBase::Connect2");
    std::lock_guard lock(magma_mutex_);
    if (!CheckSystemDevice(_completer))
      return;

    auto connection = MagmaSystemDevice::Open(magma_system_device_, request->client_id,
                                              std::move(request->primary_channel),
                                              std::move(request->notification_channel));

    if (!connection) {
      MAGMA_DLOG("MagmaSystemDevice::Open failed");
      _completer.Close(ZX_ERR_INTERNAL);
      return;
    }

    magma_system_device_->StartConnectionThread(std::move(connection),
                                                [](const char* role_name) {});
  }

  void DumpState(typename fws::DumpStateRequestView request,
                 typename fws::DumpStateCompleter::Sync& _completer) override {
    MAGMA_DLOG("MagmaDriverBase::DumpState");
    std::lock_guard lock(magma_mutex_);
    if (!CheckSystemDevice(_completer))
      return;
    if (request->dump_type & ~MAGMA_DUMP_TYPE_NORMAL) {
      MAGMA_DLOG("Invalid dump type %d", request->dump_type);
      return;
    }

    if (magma_system_device_)
      magma_system_device_->DumpStatus(request->dump_type);
  }

  void GetIcdList(typename fws::GetIcdListCompleter::Sync& completer) override {
    std::lock_guard lock(magma_mutex_);
    if (!CheckSystemDevice(completer))
      return;
    fidl::Arena allocator;
    std::vector<msd_icd_info_t> msd_icd_infos;
    magma_system_device_->GetIcdList(&msd_icd_infos);
    std::vector<fuchsia_gpu_magma::wire::IcdInfo> icd_infos;
    for (auto& item : msd_icd_infos) {
      auto icd_info = fuchsia_gpu_magma::wire::IcdInfo::Builder(allocator);
      icd_info.component_url(fidl::StringView::FromExternal(item.component_url));
      fuchsia_gpu_magma::wire::IcdFlags flags;
      if (item.support_flags & ICD_SUPPORT_FLAG_VULKAN)
        flags |= fuchsia_gpu_magma::wire::IcdFlags::kSupportsVulkan;
      if (item.support_flags & ICD_SUPPORT_FLAG_OPENCL)
        flags |= fuchsia_gpu_magma::wire::IcdFlags::kSupportsOpencl;
      if (item.support_flags & ICD_SUPPORT_FLAG_MEDIA_CODEC_FACTORY)
        flags |= fuchsia_gpu_magma::wire::IcdFlags::kSupportsMediaCodecFactory;
      icd_info.flags(flags);
      icd_infos.push_back(icd_info.Build());
    }

    completer.Reply(fidl::VectorView<fuchsia_gpu_magma::wire::IcdInfo>::FromExternal(icd_infos));
  }

 private:
  zx::result<> CreateDevfsNode() {
    fidl::Arena arena;
    zx::result connector = magma_devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      node_client_ = {};
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                     .connector(std::move(connector.value()))
                     .class_name("gpu");

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, "magma_gpu")
                    .devfs_args(devfs.Build())
                    .Build();

    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed: %s", controller_endpoints.status_string());
    zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed: %s", node_endpoints.status_string());

    fidl::WireResult result = node_client_->AddChild(args, std::move(controller_endpoints->server),
                                                     std::move(node_endpoints->server));
    gpu_node_controller_.Bind(std::move(controller_endpoints->client));
    gpu_node_.Bind(std::move(node_endpoints->client));
    return zx::ok();
  }

  void BindConnector(fidl::ServerEnd<FidlDeviceType> server) {
    fidl::BindServer(dispatcher(), std::move(server), this);
  }

  // Node representing this device; given from the parent.
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_client_;

  fit::deferred_callback teardown_logger_callback_;

  std::mutex magma_mutex_;
  std::unique_ptr<MagmaDriver> magma_driver_ FIT_GUARDED(magma_mutex_);
  std::shared_ptr<MagmaSystemDevice> magma_system_device_ FIT_GUARDED(magma_mutex_);
  driver_devfs::Connector<FidlDeviceType> magma_devfs_connector_;
  // Node representing /dev/class/gpu/<id>.
  fidl::WireSyncClient<fuchsia_driver_framework::Node> gpu_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> gpu_node_controller_;
};

class MagmaTestDriverBase : public MagmaDriverBase<fuchsia_gpu_magma::TestDevice> {
 public:
  MagmaTestDriverBase(std::string_view name, fdf::DriverStartArgs start_args,
                      fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaDriverBase(name, std::move(start_args), std::move(driver_dispatcher)) {}

  void GetUnitTestStatus(fws::GetUnitTestStatusCompleter::Sync& _completer) override {
    MAGMA_DLOG("MagmaDriverBase::GetUnitTestStatus");
    std::lock_guard<std::mutex> lock(magma_mutex());
    if (!CheckSystemDevice(_completer))
      return;
    _completer.Reply(unit_test_status_);
  }
  void set_unit_test_status(zx_status_t status) { unit_test_status_ = status; }

 private:
  zx_status_t unit_test_status_ = ZX_OK;
};

using MagmaProductionDriverBase = MagmaDriverBase<fuchsia_gpu_magma::CombinedDevice>;

#endif  // SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_CPP_MAGMA_DRIVER_BASE_H_
