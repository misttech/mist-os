// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_FORWARDER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_FORWARDER_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/zx/result.h>

#include "src/graphics/display/drivers/fake/sysmem-service-provider.h"

namespace display {

// Forwards the following sysmem protocols from the component's incoming service
// directory to the forwarder's outgoing service directory:
// - [`fuchsia.sysmem2/Allocator`]
// - [`fucshia.hardware.sysmem/Sysmem`]
class SysmemServiceForwarder : public SysmemServiceProvider {
 public:
  // Factory method for production use.
  static zx::result<std::unique_ptr<SysmemServiceForwarder>> Create();

  // Production code must use the `Create()` factory method.
  SysmemServiceForwarder();
  ~SysmemServiceForwarder() override;

  SysmemServiceForwarder(const SysmemServiceForwarder&) = delete;
  SysmemServiceForwarder& operator=(const SysmemServiceForwarder&) = delete;
  SysmemServiceForwarder(SysmemServiceForwarder&&) = delete;
  SysmemServiceForwarder& operator=(SysmemServiceForwarder&&) = delete;

  // Initialization logic not suitable for the constructor.
  zx::result<> Initialize();

  // SysmemServiceProvider:
  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> ConnectAllocator2() override;
  zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>> ConnectHardwareSysmem() override;

 private:
  fidl::ClientEnd<fuchsia_io::Directory> component_incoming_root_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_FORWARDER_H_
