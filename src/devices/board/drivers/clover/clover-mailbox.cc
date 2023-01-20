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

static const std::vector<fpbus::Mmio> mailbox0_mmios{
    {{
        .base = A1_DSPA_BASE,
        .length = A1_DSPA_BASE_LENGTH,
    }},
    {{
        .base = A1_DSPA_PAYLOAD_BASE,
        .length = A1_DSPA_PAYLOAD_BASE_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> mailbox0_irqs{
    {{
        .irq = A1_DSPA_RECV_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Mmio> mailbox1_mmios{
    {{
        .base = A1_DSPB_BASE,
        .length = A1_DSPB_BASE_LENGTH,
    }},
    {{
        .base = A1_DSPB_PAYLOAD_BASE,
        .length = A1_DSPB_PAYLOAD_BASE_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> mailbox1_irqs{
    {{
        .irq = A1_DSPB_RECV_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

zx_status_t Clover::MailboxInit() {
  fpbus::Node mailbox0_dev;
  mailbox0_dev.name() = "mailbox0";
  mailbox0_dev.vid() = PDEV_VID_AMLOGIC;
  mailbox0_dev.pid() = PDEV_PID_AMLOGIC_A1;
  mailbox0_dev.did() = PDEV_DID_AMLOGIC_MAILBOX;
  mailbox0_dev.instance_id() = 0;
  mailbox0_dev.mmio() = mailbox0_mmios;
  mailbox0_dev.irq() = mailbox0_irqs;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('MAIL');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, mailbox0_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Mailbox(mailbox0_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Mailbox(mailbox0_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  fpbus::Node mailbox1_dev;
  mailbox1_dev.name() = "mailbox1";
  mailbox1_dev.vid() = PDEV_VID_AMLOGIC;
  mailbox1_dev.pid() = PDEV_PID_AMLOGIC_A1;
  mailbox1_dev.did() = PDEV_DID_AMLOGIC_MAILBOX;
  mailbox1_dev.instance_id() = 1;
  mailbox1_dev.mmio() = mailbox1_mmios;
  mailbox1_dev.irq() = mailbox1_irqs;

  result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, mailbox1_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Mailbox(mailbox1_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Mailbox(mailbox1_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace clover
