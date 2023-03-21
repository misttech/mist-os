// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302-signals.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <cstdint>
#include <type_traits>
#include <utility>

#include "src/devices/power/drivers/fusb302/fusb302-fifos.h"
#include "src/devices/power/drivers/fusb302/fusb302-sensors.h"
#include "src/devices/power/drivers/fusb302/registers.h"
#include "src/devices/power/drivers/fusb302/usb-pd-message-type.h"
#include "src/devices/power/drivers/fusb302/usb-pd-message.h"

namespace fusb302 {

Fusb302Signals::Fusb302Signals(fidl::ClientEnd<fuchsia_hardware_i2c::Device>& i2c_channel,
                               Fusb302Sensors& sensors, Fusb302Protocol& protocol)
    : i2c_(i2c_channel), sensors_(sensors), protocol_(protocol) {}

static_assert(std::is_trivially_destructible_v<Fusb302Signals>,
              "Move non-trivial destructors outside the header");

HardwareStateChanges Fusb302Signals::ServiceInterrupts() {
  HardwareStateChanges changes = {};

  //  Read interrupts
  auto interrupt = InterruptReg::ReadFrom(i2c_);
  auto interrupt_a = InterruptAReg::ReadFrom(i2c_);
  auto interrupt_b = InterruptBReg::ReadFrom(i2c_);
  zxlogf(DEBUG, "Servicing interrupts: Interrupt 0x%02x, InterruptA 0x%02x, InterruptB 0x%02x",
         interrupt.reg_value(), interrupt_a.reg_value(), interrupt_b.reg_value());

  if (interrupt.i_vbusok()) {
    zxlogf(TRACE, "Interrupt: VBUS power good voltage comparator changed");
    if (sensors_.UpdateComparatorsResult()) {
      changes.port_state_changed = true;
    }
  }

  if (interrupt.i_comp_chng()) {
    zxlogf(TRACE, "Interrupt: variable voltage comparator output changed");
    if (sensors_.UpdateComparatorsResult()) {
      changes.port_state_changed = true;
    }
  }

  if (interrupt.i_bc_lvl()) {
    zxlogf(TRACE, "Interrupt: fixed CC voltage comparators output changed");
    if (sensors_.UpdateComparatorsResult()) {
      changes.port_state_changed = true;
    }
  }

  if (interrupt_a.i_togdone()) {
    zxlogf(TRACE, "Interrupt: hardware power role detection finished");
    if (sensors_.UpdatePowerRoleDetectionResult()) {
      changes.port_state_changed = true;
    }
  }

  if (interrupt.i_crc_chk()) {
    zxlogf(TRACE, "Interrupt: received PD message (correct CRC)");
    [[maybe_unused]] zx::result<> result = protocol_.DrainReceiveFifo();
  }

  // This interrupt must be processed after the receive interrupt, so the PD
  // protocol layer learns it doesn't need to send a GoodCRC anymore.
  if (interrupt_b.i_gcrcsent()) {
    zxlogf(TRACE, "Interrupt: sent hardware-generated GoodCRC");
    protocol_.DidTransmitGoodCrc();
  }

  // Normalize Soft Reset messages to Soft Reset interrupts.
  if (protocol_.HasUnreadMessage()) {
    // Soft Reset messages cause all previously received messages to be
    // dropped. So, if we receive a Soft Reset, it must be the first message
    // in the queue.
    if (protocol_.FirstUnreadMessage().header().message_type() == usb_pd::MessageType::kSoftReset) {
      zxlogf(TRACE, "Converting Soft Reset received message to soft reset notice");
      changes.received_reset = true;

      [[maybe_unused]] zx::result<> result = protocol_.MarkMessageAsRead();
    }
  }

  if (interrupt_a.i_hardrst()) {
    zxlogf(ERROR, "Interrupt: received a Hard Reset ordered set. We'll lose power soon!");
    changes.received_reset = true;
  }

  if (interrupt_a.i_retryfail()) {
    zxlogf(ERROR,
           "Interrupt: timed out waiting for GoodCRC. Transmitted message keeps getting lost.");
    protocol_.DidTimeoutWaitingForGoodCrc();
  }

  // Log errors that shouldn't happen.

  if (interrupt.i_alert()) {
    zxlogf(TRACE, "Interrupt: PHY error");
    auto status1 = Status1Reg::ReadFrom(i2c_);
    zxlogf(ERROR, "PHY error: TX queue %s, RX queue %s", status1.tx_full() ? "full" : "ok",
           status1.rx_full() ? "full" : "ok");
  }

  if (interrupt.i_collision()) {
    zxlogf(ERROR,
           "BMC PHY discarded transmission due to CC activity. PD collision avoidance failed.");
  }

  if (interrupt_a.i_ocp_temp()) {
    zxlogf(TRACE, "Interrupt: thermal alert");
    auto status1 = Status1Reg::ReadFrom(i2c_);
    zxlogf(ERROR, "Thermal alert! Junction temperature %s, VCONN over-protection %s",
           status1.ovrtemp() ? "too high" : "ok", status1.ocp() ? "tripped" : "ok");
  }

  if (interrupt_a.i_hardsent()) {
    zxlogf(ERROR, "Interrupt: transmitted  a Hard Reset ordered set. We'll lose power soon!");
  }

  return changes;
}

zx::result<> Fusb302Signals::InitInterruptUnit() {
  // The interrupts enabled here must be kept in sync with the interrupts
  // serviced in `ServiceInterrupts()`.

  zx_status_t status = MaskReg::FromAllInterruptsMasked()
                           .set_m_vbusok(false)
                           .set_m_comp_chng(false)
                           .set_m_crc_chk(false)
                           .set_m_alert(false)
                           .set_m_collision(false)
                           .set_m_bc_lvl(false)
                           .WriteTo(i2c_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to write Mask register: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Experiments with a FUSB302BMPX indicate that the "GoodCRC received" and
  // "Soft Reset received" interrupts are redundant with processing GoodCRC
  // messages in the Rx (receive) FIFO. We have to process the Rx FIFO for other
  // messages, so we don't use these interrupts.
  status = MaskAReg::FromAllInterruptsMasked()
               .set_m_ocp_temp(false)
               .set_m_togdone(false)
               .set_m_retryfail(false)
               .set_m_hardsent(false)
               .set_m_hardrst(false)
               .WriteTo(i2c_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to write MaskA register: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::result<> result =
      MaskBReg::ReadModifyWrite(i2c_, [&](MaskBReg& mask_b) { mask_b.set_m_gcrcsent(false); });
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to write MaskB register: %s", result.status_string());
    return result;
  }

  // Clear any old interrupt requests.
  [[maybe_unused]] auto interrupts = InterruptReg::ReadFrom(i2c_);
  [[maybe_unused]] auto interrupts_a = InterruptAReg::ReadFrom(i2c_);
  [[maybe_unused]] auto interrupts_b = InterruptBReg::ReadFrom(i2c_);

  return zx::ok();
}

}  // namespace fusb302
