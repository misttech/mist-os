// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_DEVICE_IMPL_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_DEVICE_IMPL_H_

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/ddk/device.h>
#include <lib/fit/thread_safety.h>

#include <ddktl/device.h>

#include "platform_thread.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/magma_dependency_injection_device.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/magma_performance_counter_device.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_status.h"
#include "sys_driver/magma_driver.h"

namespace magma {
#if MAGMA_TEST_DRIVER
using DeviceType = fuchsia_gpu_magma::TestDevice;
#else
using DeviceType = fuchsia_gpu_magma::CombinedDevice;
#endif

// This class implements all FIDL methods exported by MSDs. ddk::Devices should depend on it as a
// mixin. `D` is the type of the subclass (CRTP), which will be supplied by ddk::Device. The class
// is thread-safe.
template <typename D>
class MagmaDeviceImpl : public ddk::Messageable<DeviceType>::Mixin<D>,
                        public magma::MagmaDependencyInjectionDevice::Owner {
 public:
  using fws = fidl::WireServer<DeviceType>;

  void set_zx_device(zx_device_t* zx_device) {
    ZX_DEBUG_ASSERT(!zx_device_);
    zx_device_ = zx_device;
  }

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
#if MAGMA_TEST_DRIVER
  void set_unit_test_status(zx_status_t status) { unit_test_status_ = status; }
#endif

  zx_status_t MagmaStop() {
    std::lock_guard lock(magma_mutex_);
    magma_system_device_->Shutdown();
    magma_system_device_.reset();
    return ZX_OK;
  }

  // Initialize child devices that are used for various purposes. Must be called exactly once,
  // within DdkInit().
  zx_status_t InitChildDevices() {
    std::lock_guard<std::mutex> lock(magma_mutex_);
    ZX_DEBUG_ASSERT(zx_device_);
    if (!magma::MagmaPerformanceCounterDevice::AddDevice(zx_device_, &perf_counter_koid_)) {
      return ZX_ERR_INTERNAL;
    }

    magma_system_device_->set_perf_count_access_token_id(perf_counter_koid_);

    auto dependency_injection_device =
        std::make_unique<magma::MagmaDependencyInjectionDevice>(zx_device_, this);
    if (magma::MagmaDependencyInjectionDevice::Bind(std::move(dependency_injection_device)) !=
        ZX_OK) {
      return ZX_ERR_INTERNAL;
    }
    return ZX_OK;
  }

  // magma::MagmaDependencyInjection::Owner implementation.
  void SetMemoryPressureLevel(MagmaMemoryPressureLevel level) override {
    std::lock_guard lock(magma_mutex_);
    last_memory_pressure_level_ = level;
    if (magma_system_device_)
      magma_system_device_->SetMemoryPressureLevel(level);
  }

  // Initialize magma_system_device_ on creation.
  void InitSystemDevice() FIT_REQUIRES(magma_mutex_) {
    magma_system_device_->set_perf_count_access_token_id(perf_counter_koid_);
    if (last_memory_pressure_level_) {
      magma_system_device_->SetMemoryPressureLevel(*last_memory_pressure_level_);
    }
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

  void Query(fws::QueryRequestView request, fws::QueryCompleter::Sync& _completer) override {
    DLOG("MagmaDeviceImpl::Query");
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

  void Connect2(fws::Connect2RequestView request,
                fws::Connect2Completer::Sync& _completer) override {
    DLOG("MagmaDeviceImpl::Connect2");
    std::lock_guard lock(magma_mutex_);
    if (!CheckSystemDevice(_completer))
      return;

    auto connection = MagmaSystemDevice::Open(magma_system_device_, request->client_id,
                                              std::move(request->primary_channel),
                                              std::move(request->notification_channel));

    if (!connection) {
      DLOG("MagmaSystemDevice::Open failed");
      _completer.Close(ZX_ERR_INTERNAL);
      return;
    }

    ZX_DEBUG_ASSERT(zx_device_);
    magma_system_device_->StartConnectionThread(
        std::move(connection), [zx_device_ = zx_device_](const char* name) {
          magma::PlatformThreadHelper::SetRole(zx_device_, name);
        });
  }

  void DumpState(fws::DumpStateRequestView request,
                 fws::DumpStateCompleter::Sync& _completer) override {
    DLOG("MagmaDeviceImpl::DumpState");
    std::lock_guard lock(magma_mutex_);
    if (!CheckSystemDevice(_completer))
      return;
    if (request->dump_type & ~MAGMA_DUMP_TYPE_NORMAL) {
      DLOG("Invalid dump type %d", request->dump_type);
      return;
    }

    if (magma_system_device_)
      magma_system_device_->DumpStatus(request->dump_type);
  }

  void GetIcdList(fws::GetIcdListCompleter::Sync& completer) override {
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

#if MAGMA_TEST_DRIVER
  void GetUnitTestStatus(fws::GetUnitTestStatusCompleter::Sync& _completer) override {
    DLOG("MagmaDeviceImpl::GetUnitTestStatus");
    std::lock_guard<std::mutex> lock(magma_mutex_);
    if (!CheckSystemDevice(_completer))
      return;
    _completer.Reply(unit_test_status_);
  }
#endif  // MAGMA_TEST_DRIVER

 private:
  std::mutex magma_mutex_;
  std::unique_ptr<MagmaDriver> magma_driver_ FIT_GUARDED(magma_mutex_);
  std::shared_ptr<MagmaSystemDevice> magma_system_device_ FIT_GUARDED(magma_mutex_);
  zx_device_t* zx_device_ = nullptr;
  zx_koid_t perf_counter_koid_ = 0;
  std::optional<MagmaMemoryPressureLevel> last_memory_pressure_level_ FIT_GUARDED(magma_mutex_);
#if MAGMA_TEST_DRIVER
  zx_status_t unit_test_status_ = ZX_ERR_NOT_SUPPORTED;
#endif
};

}  // namespace magma

#endif  // SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_DEVICE_IMPL_H_
