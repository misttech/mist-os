// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VSYNC_RECEIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VSYNC_RECEIVER_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/async/cpp/irq.h>
#include <lib/ddk/device.h>
#include <lib/fit/function.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <threads.h>
#include <zircon/syscalls/port.h>

#include <cstddef>
#include <memory>

#include <fbl/mutex.h>

#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher-factory.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher.h"

namespace amlogic_display {

// Receives Vertical Sync (Vsync) interrupts triggered by the display engine
// indicating that the display engine finishes presenting a frame to the
// display device.
class VsyncReceiver {
 public:
  // Internal state size for the function called when a Vsync interrupt is
  // triggered.
  static constexpr size_t kOnVsyncTargetSize = 16;

  // The type of the function called when a Vsync interrupt is triggered.
  using VsyncHandler = fit::inline_function<void(zx::time timestamp), kOnVsyncTargetSize>;

  // Factory method intended for production use.
  //
  // `platform_device` must be valid.
  //
  // `on_vsync` is called when the display engine finishes presenting a frame
  // to the display device and triggers a Vsync interrupt. Must be non-null.
  static zx::result<std::unique_ptr<VsyncReceiver>> Create(
      display::DispatcherFactory& dispatcher_factory,
      fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device,
      VsyncHandler on_vsync);

  // Production code should prefer the factory method `Create()`.
  //
  // `irq_handler_dispatcher` must not be empty.
  // `vsync_irq` must be valid.
  // `on_vsync` must be non-null.
  explicit VsyncReceiver(zx::interrupt vsync_irq, VsyncHandler on_vsync,
                         std::unique_ptr<display::Dispatcher> irq_handler_dispatcher);

  VsyncReceiver(const VsyncReceiver&) = delete;
  VsyncReceiver& operator=(const VsyncReceiver&) = delete;

  ~VsyncReceiver();

  // Initialization work that is not suitable for the constructor.
  //
  // Called by Create().
  zx::result<> Init();

 private:
  void OnVsync(zx::time timestamp);

  void InterruptHandler(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                        const zx_packet_interrupt_t* interrupt);

  const zx::interrupt vsync_irq_;

  const VsyncHandler on_vsync_;

  // The `irq_handler_dispatcher_` and `irq_handler_` are constant between
  // Init() and instance destruction. Only accessed on the threads used for
  // class initialization and destruction.
  std::unique_ptr<display::Dispatcher> irq_handler_dispatcher_;
  async::IrqMethod<VsyncReceiver, &VsyncReceiver::InterruptHandler> irq_handler_{this};
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VSYNC_RECEIVER_H_
