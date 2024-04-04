// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/vsync-receiver.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/amlogic-display/board-resources.h"
#include "src/graphics/display/drivers/amlogic-display/irq-handler-loop-util.h"

namespace amlogic_display {

// static
zx::result<std::unique_ptr<VsyncReceiver>> VsyncReceiver::Create(
    zx_device_t* parent,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device,
    VsyncHandler on_vsync) {
  ZX_DEBUG_ASSERT(platform_device.is_valid());

  zx::result<zx::interrupt> vsync_irq_result =
      GetInterrupt(InterruptResourceIndex::kViu1Vsync, platform_device);
  if (vsync_irq_result.is_error()) {
    return vsync_irq_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto vsync_receiver = fbl::make_unique_checked<VsyncReceiver>(
      &alloc_checker, std::move(vsync_irq_result).value(), std::move(on_vsync));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Out of memory while allocating VsyncReceiver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> init_result = vsync_receiver->Init(parent);
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to initalize VsyncReceiver: %s", init_result.status_string());
    return init_result.take_error();
  }

  return zx::ok(std::move(vsync_receiver));
}

VsyncReceiver::VsyncReceiver(zx::interrupt vsync_irq, VsyncHandler on_vsync)
    : vsync_irq_(std::move(vsync_irq)),
      on_vsync_(std::move(on_vsync)),
      irq_handler_loop_config_(CreateIrqHandlerAsyncLoopConfig()),
      irq_handler_loop_(&irq_handler_loop_config_) {
  irq_handler_.set_object(vsync_irq_.get());
}

VsyncReceiver::~VsyncReceiver() {
  // In order to shut down the interrupt handler and join the thread, the
  // interrupt must be destroyed first.
  if (vsync_irq_.is_valid()) {
    zx_status_t status = vsync_irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(ERROR, "VsyncReceiver done interrupt destroy failed: %s",
             zx_status_get_string(status));
    }
  }

  irq_handler_loop_.Shutdown();
}

zx::result<> VsyncReceiver::Init(zx_device_t* parent) {
  zx_status_t status =
      irq_handler_loop_.StartThread("vsync-interrupt-thread", &irq_handler_thread_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start a thread for the Vsync handler loop: %s",
           zx_status_get_string(status));
    return zx::error(status);
  }

  // Set scheduler role for the vsync interrupt handler thread.
  static constexpr char kRoleName[] = "fuchsia.graphics.display.drivers.amlogic-display.vsync";
  status = device_set_profile_by_role(parent, thrd_get_zx_handle(irq_handler_thread_), kRoleName,
                                      strlen(kRoleName));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to apply role: %s", zx_status_get_string(status));
  }

  status = irq_handler_.Begin(irq_handler_loop_.dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to bind the Vsync handler to the async loop: %s",
           zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void VsyncReceiver::InterruptHandler(async_dispatcher_t* dispatcher, async::IrqBase* irq,
                                     zx_status_t status, const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    zxlogf(INFO, "Vsync interrupt wait is cancelled.");
    return;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "Vsync interrupt wait failed: %s", zx_status_get_string(status));
    // A failed async interrupt wait doesn't remove the interrupt from the
    // async loop, so we have to manually cancel it.
    irq->Cancel();
    return;
  }

  OnVsync(zx::time(interrupt->timestamp));

  // For interrupts bound to ports (including those bound to async loops), the
  // interrupt must be re-armed using zx_interrupt_ack() for each incoming
  // interrupt request. This is best done after the interrupt has been fully
  // processed.
  zx::unowned_interrupt(irq->object())->ack();
}

void VsyncReceiver::OnVsync(zx::time timestamp) { on_vsync_(timestamp); }

}  // namespace amlogic_display
