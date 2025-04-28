// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>

#include <soc/aml-common/aml-registers.h>

#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-regs.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy2-regs.h"

namespace aml_usb_phy {

namespace {

void dump_usb_regs(const fdf::MmioBuffer& mmio) {
  DUMP_REG(USB_R0_V2, mmio)
  DUMP_REG(USB_R1_V2, mmio)
  DUMP_REG(USB_R2_V2, mmio)
  DUMP_REG(USB_R3_V2, mmio)
  DUMP_REG(USB_R4_V2, mmio)
  DUMP_REG(USB_R5_V2, mmio)
  DUMP_REG(USB_R6_V2, mmio)
}

}  // namespace

void AmlUsbPhy::dump_regs() {
  dump_usb_regs(usbctrl_mmio_);

  for (const auto& u2 : usbphy2_) {
    u2.dump_regs();
  }
  for (const auto& u3 : usbphy3_) {
    u3.dump_regs();
  }
}

zx_status_t AmlUsbPhy::InitPhy2() {
  auto* usbctrl_mmio = &usbctrl_mmio_;

  // first reset USB
  auto portnum = usbphy2_.size();
  uint32_t reset_level = 0;
  while (portnum) {
    portnum--;
    reset_level = reset_level | (1 << (16 + portnum));
  }

  auto level_result =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, reset_level, reset_level);
  if ((level_result.status() != ZX_OK) || level_result->is_error()) {
    FDF_LOG(ERROR, "Reset Level Write failed\n");
    return ZX_ERR_INTERNAL;
  }

  // amlogic_new_usbphy_reset_v2()
  auto register_result1 = reset_register_->WriteRegister32(
      RESET1_REGISTER_OFFSET, aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK,
      aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK);
  if ((register_result1.status() != ZX_OK) || register_result1->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 2 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  // amlogic_new_usb2_init()
  for (auto& phy : usbphy2_) {
    auto u2p_ro_v2 = U2P_R0_V2::Get(phy.idx()).ReadFrom(usbctrl_mmio);
    if (phy.is_otg_capable()) {
      u2p_ro_v2.set_idpullup0(1).set_drvvbus0(1);
    }
    u2p_ro_v2.set_por(1)
        .set_host_device(phy.dr_mode() != fuchsia_hardware_usb_phy::Mode::kPeripheral)
        .WriteTo(usbctrl_mmio);
    u2p_ro_v2.set_por(0).WriteTo(usbctrl_mmio);
  }

  zx::nanosleep(zx::deadline_after(zx::usec(10)));

  // amlogic_new_usbphy_reset_phycfg_v2()
  auto register_result2 =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, reset_level, ~reset_level);
  if ((register_result2.status() != ZX_OK) || register_result2->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 16 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  auto register_result3 =
      reset_register_->WriteRegister32(RESET1_LEVEL_OFFSET, aml_registers::USB_RESET1_LEVEL_MASK,
                                       aml_registers::USB_RESET1_LEVEL_MASK);
  if ((register_result3.status() != ZX_OK) || register_result3->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 16 failed\n");
    return ZX_ERR_INTERNAL;
  }

  zx::nanosleep(zx::deadline_after(zx::usec(50)));

  for (auto& phy : usbphy2_) {
    auto mmio = &phy.mmio();
    PHY2_R21::Get().ReadFrom(mmio).set_usb2_otg_aca_en(0).WriteTo(mmio);

    auto u2p_r1 = U2P_R1_V2::Get(phy.idx());
    int count = 0;
    while (!u2p_r1.ReadFrom(usbctrl_mmio).phy_rdy()) {
      // wait phy ready max 5ms, common is 100us
      if (count > 1000) {
        FDF_LOG(WARNING, "AmlUsbPhy::InitPhy U2P_R1_PHY_RDY wait failed");
        break;
      }

      count++;
      zx::nanosleep(zx::deadline_after(zx::usec(5)));
    }
  }

  // One time PLL initialization
  for (auto& phy : usbphy2_) {
    phy.InitPll(type_, needs_hack_);
  }

  return ZX_OK;
}

zx_status_t AmlUsbPhy::InitOtg() {
  auto* mmio = &usbctrl_mmio_;

  USB_R1_V2::Get().ReadFrom(mmio).set_u3h_fladj_30mhz_reg(0x20).WriteTo(mmio);

  USB_R5_V2::Get().ReadFrom(mmio).set_iddig_en0(1).set_iddig_en1(1).set_iddig_th(255).WriteTo(mmio);

  return ZX_OK;
}

zx_status_t AmlUsbPhy::InitPhy3() {
  for (auto& usbphy3 : usbphy3_) {
    auto status = usbphy3.Init(usbctrl_mmio_);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "usbphy3.Init() error %s", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

void AmlUsbPhy::ChangeMode(UsbPhyBase& phy, fuchsia_hardware_usb_phy::Mode new_mode) {
  auto old_mode = phy.phy_mode();
  if (new_mode == old_mode) {
    FDF_LOG(ERROR, "Already in %d mode", static_cast<uint32_t>(new_mode));
    return;
  }
  phy.SetMode(new_mode, usbctrl_mmio_);

  if (new_mode == fuchsia_hardware_usb_phy::Mode::kHost) {
    ++controller_->xhci_;
    if (old_mode != fuchsia_hardware_usb_phy::Mode::kUnknown) {
      --controller_->dwc2_;
    }
  } else {
    ++controller_->dwc2_;
    if (old_mode != fuchsia_hardware_usb_phy::Mode::kUnknown) {
      --controller_->xhci_;
    }
  }
}

void AmlUsbPhy::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                          const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    return;
  }
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "irq_.wait failed: %d", status);
    return;
  }

  {
    auto r5 = USB_R5_V2::Get().ReadFrom(&usbctrl_mmio_);
    // Acknowledge interrupt
    r5.set_usb_iddig_irq(0).WriteTo(&usbctrl_mmio_);

    // Read current host/device role.
    for (auto& phy : usbphy2_) {
      if (phy.dr_mode() != fuchsia_hardware_usb_phy::Mode::kOtg) {
        continue;
      }

      ChangeMode(phy, r5.iddig_curr() == 0 ? fuchsia_hardware_usb_phy::Mode::kHost
                                           : fuchsia_hardware_usb_phy::Mode::kPeripheral);
    }
  }

  irq_.ack();
}

zx_status_t AmlUsbPhy::Init() {
  bool has_otg = false;
  auto status = InitPhy2();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitPhy2() error %s", zx_status_get_string(status));
    return status;
  }
  status = InitOtg();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitOtg() error %s", zx_status_get_string(status));
    return status;
  }
  status = InitPhy3();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitPhy3() error %s", zx_status_get_string(status));
    return status;
  }

  for (auto& phy : usbphy2_) {
    fuchsia_hardware_usb_phy::Mode mode;
    if (phy.dr_mode() != fuchsia_hardware_usb_phy::Mode::kOtg) {
      mode = phy.dr_mode() == fuchsia_hardware_usb_phy::Mode::kHost
                 ? fuchsia_hardware_usb_phy::Mode::kHost
                 : fuchsia_hardware_usb_phy::Mode::kPeripheral;
    } else {
      has_otg = true;
      // Wait for PHY to stabilize before reading initial mode.
      zx::nanosleep(zx::deadline_after(zx::sec(1)));
      mode = USB_R5_V2::Get().ReadFrom(&usbctrl_mmio_).iddig_curr() == 0
                 ? fuchsia_hardware_usb_phy::Mode::kHost
                 : fuchsia_hardware_usb_phy::Mode::kPeripheral;
    }

    ChangeMode(phy, mode);
  }

  for (auto& phy : usbphy3_) {
    if (phy.dr_mode() != fuchsia_hardware_usb_phy::Mode::kHost) {
      FDF_LOG(ERROR, "Not support USB3 in non-host mode yet");
    }

    ChangeMode(phy, fuchsia_hardware_usb_phy::Mode::kHost);
  }

  if (has_otg) {
    irq_handler_.set_object(irq_.get());
    auto status = irq_handler_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
    if (status != ZX_OK) {
      return ZX_ERR_INTERNAL;
    }

    return ZX_OK;
  }

  return ZX_OK;
}

// PHY tuning based on connection state
void AmlUsbPhy::ConnectStatusChanged(ConnectStatusChangedRequest& request,
                                     ConnectStatusChangedCompleter::Sync& completer) {
  // Handled by UTMI bus
  dwc2_connected_ = request.connected();
  completer.Reply(fit::ok());
}

}  // namespace aml_usb_phy
