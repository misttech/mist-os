// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>

#include <soc/aml-a1/a1-hw.h>

#include "clover.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace clover {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> mailbox_mmios{
    {{
        .base = A1_DSPA_BASE,
        .length = A1_DSPA_BASE_LENGTH,
    }},
    {{
        .base = A1_DSPA_PAYLOAD_BASE,
        .length = A1_DSPA_PAYLOAD_BASE_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> mailbox_irqs{
    {{
        .irq = A1_DSPA_RECV_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

zx_status_t Clover::MailboxInit() {
  fpbus::Node mailbox_dev;
  mailbox_dev.name() = "mailbox";
  mailbox_dev.vid() = PDEV_VID_AMLOGIC;
  mailbox_dev.pid() = PDEV_PID_AMLOGIC_A1;
  mailbox_dev.did() = PDEV_DID_AMLOGIC_MAILBOX;
  mailbox_dev.mmio() = mailbox_mmios;
  mailbox_dev.irq() = mailbox_irqs;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('MAIL');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, mailbox_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Mailbox(mailbox_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Mailbox(mailbox_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace clover
