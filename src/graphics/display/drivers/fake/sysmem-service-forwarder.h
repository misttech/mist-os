// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_FORWARDER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_FORWARDER_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/zx/result.h>

#include "src/graphics/display/drivers/fake/sysmem-service-provider.h"

namespace display {

// Forwards the sysmem protocol from the component's incoming service
// directory to the [`fuchsia.hardware.sysmem/Service`] service served on the
// forwarder's outgoing service directory.
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
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> GetOutgoingDirectory() override;

 private:
  // The reteurned instance handler must be used only when the
  // `SysmemServiceForwarder` instance is alive.
  fuchsia_hardware_sysmem::Service::InstanceHandler CreateSysmemServiceInstanceHandler() const;

  // Serves the outgoing services. Must outlive `outgoing_`.
  async::Loop loop_;

  // Must be created, called and destroyed on `loop_`.
  std::optional<component::OutgoingDirectory> outgoing_;

  fidl::ClientEnd<fuchsia_io::Directory> component_incoming_root_;
  fidl::ClientEnd<fuchsia_io::Directory> forwarder_outgoing_root_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_FORWARDER_H_
