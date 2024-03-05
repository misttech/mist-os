// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_H_
#define SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_H_

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/async/cpp/irq.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy2.h"
#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy3.h"
#include "src/devices/usb/drivers/vim3-usb-phy/vim3-usb-phy-device.h"

namespace vim3_usb_phy {

// This is the main class for the platform bus driver.
// The Vim3UsbPhy driver manages 3 different controllers:
//  - One USB 2.0 controller that is only supports host mode.
//  - One USB 2.0 controller that supports OTG (both host and device mode).
//  - One USB 3.0 controller that only supports host mode.
class Vim3UsbPhy : public fdf::Server<fuchsia_hardware_usb_phy::UsbPhy> {
 public:
  Vim3UsbPhy(Vim3UsbPhyDevice* controller,
             fidl::ClientEnd<fuchsia_hardware_registers::Device> reset_register,
             std::array<uint32_t, 8> pll_settings, fdf::MmioBuffer usbctrl_mmio, zx::interrupt irq,
             std::vector<UsbPhy2> usbphy2, std::vector<UsbPhy3> usbphy3)
      : controller_(controller),
        reset_register_(std::move(reset_register)),
        usbctrl_mmio_(std::move(usbctrl_mmio)),
        usbphy2_(std::move(usbphy2)),
        usbphy3_(std::move(usbphy3)),
        irq_(std::move(irq)),
        pll_settings_(pll_settings) {}
  ~Vim3UsbPhy() override {
    irq_handler_.Cancel();
    irq_.destroy();
  }

  zx_status_t Init(bool has_otg);

  // fuchsia_hardware_usb_phy::UsbPhy required methods
  void ConnectStatusChanged(ConnectStatusChangedRequest& request,
                            ConnectStatusChangedCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_phy::UsbPhy> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FDF_LOG(ERROR, "Unknown method %lu", metadata.method_ordinal);
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(Vim3UsbPhy);

  zx_status_t InitPhy2();
  zx_status_t InitPhy3();
  zx_status_t InitOtg();

  void ChangeMode(UsbPhyBase& phy, UsbMode new_mode);

  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  Vim3UsbPhyDevice* controller_;

  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_register_;
  fdf::MmioBuffer usbctrl_mmio_;
  std::vector<UsbPhy2> usbphy2_;
  std::vector<UsbPhy3> usbphy3_;

  zx::interrupt irq_;
  async::IrqMethod<Vim3UsbPhy, &Vim3UsbPhy::HandleIrq> irq_handler_{this};

  // Magic numbers for PLL from metadata
  const std::array<uint32_t, 8> pll_settings_;

  bool dwc2_connected_ = false;
};

}  // namespace vim3_usb_phy

#endif  // SRC_DEVICES_USB_DRIVERS_VIM3_USB_PHY_VIM3_USB_PHY_H_
