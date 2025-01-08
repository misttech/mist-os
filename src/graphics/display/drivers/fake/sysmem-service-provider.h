// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_PROVIDER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_PROVIDER_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/zx/result.h>

namespace display {

// Interface for a component that exposes Sysmem.
class SysmemServiceProvider {
 public:
  SysmemServiceProvider() = default;
  virtual ~SysmemServiceProvider() = default;

  SysmemServiceProvider(const SysmemServiceProvider&) = delete;
  SysmemServiceProvider& operator=(const SysmemServiceProvider&) = delete;
  SysmemServiceProvider(SysmemServiceProvider&&) = delete;
  SysmemServiceProvider& operator=(SysmemServiceProvider&&) = delete;

  virtual zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> ConnectAllocator2() = 0;
  virtual zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>> ConnectHardwareSysmem() = 0;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_PROVIDER_H_
