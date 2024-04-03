// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_DEVICE_WRAPPER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_DEVICE_WRAPPER_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/ddk/device.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>
#include <type_traits>

#include "src/devices/sysmem/drivers/sysmem/driver.h"

namespace display {

// Clients of FakeDisplayStack pass a SysmemDeviceWrapper into the constructor to provide a
// sysmem implementation to the display driver, with the goal of supporting the following use cases:
//   - display driver unit tests want to use a self-contained/hermetic sysmem implementation, to
//     help cover error paths.
//   - system integration tests may want to use the "global" sysmem so that multiple components
//     can use it to coordinate memory allocation, for example tests which involve Scenic, Magma,
//     and the display driver.
class SysmemDeviceWrapper {
 public:
  virtual ~SysmemDeviceWrapper() = default;

  // The returned directory is guaranteed to have the
  // [`fuchsia.hardware.sysmem/Service`] service on success.
  virtual zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CloneServiceDirClient() = 0;
  virtual zx_status_t Bind() = 0;
};

// Convenient implementation of SysmemDeviceWrapper which can be used to wrap both
// sysmem_device::Driver and display::SysmemProxyDevice (the initial two usages of
// SysmemDeviceWrapper).
template <typename T>
class GenericSysmemDeviceWrapper : public SysmemDeviceWrapper {
 public:
  explicit GenericSysmemDeviceWrapper(zx_device_t* parent)
      : sysmem_ctx_(std::make_unique<sysmem_driver::Driver>()),
        owned_sysmem_(std::make_unique<T>(parent, sysmem_ctx_.get())) {
    sysmem_ = owned_sysmem_.get();
  }

  // The returned directory is guaranteed to have the
  // [`fuchsia.hardware.sysmem/Service`] service on success.
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CloneServiceDirClient() override {
    return sysmem_->CloneServiceDirClientForTests();
  }
  zx_status_t Bind() override {
    zx_status_t status = sysmem_->Bind();
    if (status == ZX_OK) {
      // DDK takes ownership of sysmem and DdkRelease will release it.
      owned_sysmem_.release();
    }
    return status;
  }

 private:
  std::unique_ptr<sysmem_driver::Driver> sysmem_ctx_;
  std::unique_ptr<T> owned_sysmem_;
  T* sysmem_{};
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_DEVICE_WRAPPER_H_
