// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/vim3-usb-phy/vim3-usb-phy.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/ddk/binding_priv.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <fbl/auto_lock.h>
#include <soc/aml-common/aml-registers.h>

#include "src/devices/usb/drivers/vim3-usb-phy/usb-phy-regs.h"

namespace vim3_usb_phy {

namespace {

struct PhyMetadata {
  std::array<uint32_t, 8> pll_settings;
  usb_mode_t dr_mode = USB_MODE_OTG;
};

zx::result<PhyMetadata> ParseMetadata(
    const fidl::VectorView<fuchsia_driver_compat::wire::Metadata>& metadata) {
  PhyMetadata parsed_metadata;
  bool found_pll_settings = false;
  for (const auto& m : metadata) {
    if (m.type == DEVICE_METADATA_PRIVATE) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size %s", zx_status_get_string(status));
        continue;
      }

      if (size != sizeof(PhyMetadata::pll_settings)) {
        FDF_LOG(ERROR, "Unexpected metadata size: got %zu, expected %zu", size, sizeof(uint32_t));
        continue;
      }

      status =
          m.data.read(parsed_metadata.pll_settings.data(), 0, sizeof(parsed_metadata.pll_settings));
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read %s", zx_status_get_string(status));
        continue;
      }

      found_pll_settings = true;
    }

    if (m.type == DEVICE_METADATA_USB_MODE) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size %s", zx_status_get_string(status));
        continue;
      }

      if (size != sizeof(parsed_metadata.dr_mode)) {
        FDF_LOG(ERROR, "Unexpected metadata size: got %zu, expected %zu", size, sizeof(uint32_t));
        continue;
      }

      status = m.data.read(&parsed_metadata.dr_mode, 0, sizeof(parsed_metadata.dr_mode));
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read %s", zx_status_get_string(status));
        continue;
      }
    }
  }

  if (found_pll_settings) {
    return zx::ok(parsed_metadata);
  }

  FDF_LOG(ERROR, "Failed to parse metadata. Metadata needs to at least have pll_settings.");
  return zx::error(ZX_ERR_NOT_FOUND);
}

}  // namespace

// Based on set_usb_pll() in phy-aml-new-usb2-v2.c
void Vim3UsbPhy::InitPll(fdf::MmioBuffer* mmio) {
  PLL_REGISTER_40::Get()
      .FromValue(0)
      .set_value(pll_settings_[0])
      .set_enable(1)
      .set_reset(1)
      .WriteTo(mmio);

  PLL_REGISTER::Get(0x44).FromValue(pll_settings_[1]).WriteTo(mmio);

  PLL_REGISTER::Get(0x48).FromValue(pll_settings_[2]).WriteTo(mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  PLL_REGISTER_40::Get()
      .FromValue(0)
      .set_value(pll_settings_[0])
      .set_enable(1)
      .set_reset(0)
      .WriteTo(mmio);

  // Phy tuning for G12B
  PLL_REGISTER::Get(0x50).FromValue(pll_settings_[3]).WriteTo(mmio);

  PLL_REGISTER::Get(0x54).FromValue(0x2a).WriteTo(mmio);

  PLL_REGISTER::Get(0x34).FromValue(0x70000).WriteTo(mmio);

  // Disconnect threshold
  PLL_REGISTER::Get(0xc).FromValue(0x34).WriteTo(mmio);
}

zx_status_t Vim3UsbPhy::CrBusAddr(uint32_t addr) {
  auto* usbphy3_mmio = &usbphy3_mmio_;

  auto phy3_r4 = PHY3_R4::Get().FromValue(0).set_phy_cr_data_in(addr);
  phy3_r4.WriteTo(usbphy3_mmio);
  phy3_r4.set_phy_cr_cap_addr(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  phy3_r4.set_phy_cr_cap_addr(1);
  phy3_r4.WriteTo(usbphy3_mmio);

  auto timeout = zx::deadline_after(zx::msec(1000));
  auto phy3_r5 = PHY3_R5::Get().FromValue(0);
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 0 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 1) {
    FDF_LOG(WARNING, "Read set cap addr for addr %x timed out", addr);
  }

  phy3_r4.set_phy_cr_cap_addr(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  timeout = zx::deadline_after(zx::msec(1000));
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 1 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 0) {
    FDF_LOG(WARNING, "Read cap addr for addr %x timed out", addr);
  }

  return ZX_OK;
}

uint32_t Vim3UsbPhy::CrBusRead(uint32_t addr) {
  auto* usbphy3_mmio = &usbphy3_mmio_;

  CrBusAddr(addr);

  auto phy3_r4 = PHY3_R4::Get().FromValue(0);
  phy3_r4.set_phy_cr_read(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  phy3_r4.set_phy_cr_read(1);
  phy3_r4.WriteTo(usbphy3_mmio);

  auto timeout = zx::deadline_after(zx::msec(1000));
  auto phy3_r5 = PHY3_R5::Get().FromValue(0);
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 0 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 1) {
    FDF_LOG(WARNING, "Read set for addr %x timed out", addr);
  }

  uint32_t data = phy3_r5.phy_cr_data_out();

  phy3_r4.set_phy_cr_read(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  timeout = zx::deadline_after(zx::msec(1000));
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 1 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 0) {
    FDF_LOG(WARNING, "Read for addr %x timed out", addr);
  }

  return data;
}

zx_status_t Vim3UsbPhy::CrBusWrite(uint32_t addr, uint32_t data) {
  auto* usbphy3_mmio = &usbphy3_mmio_;

  CrBusAddr(addr);

  auto phy3_r4 = PHY3_R4::Get().FromValue(0);
  phy3_r4.set_phy_cr_data_in(data);
  phy3_r4.WriteTo(usbphy3_mmio);

  phy3_r4.set_phy_cr_cap_data(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  phy3_r4.set_phy_cr_cap_data(1);
  phy3_r4.WriteTo(usbphy3_mmio);

  auto timeout = zx::deadline_after(zx::msec(1000));
  auto phy3_r5 = PHY3_R5::Get().FromValue(0);
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 0 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 1) {
    FDF_LOG(WARNING, "Write cap data for addr %x timed out", addr);
  }

  phy3_r4.set_phy_cr_cap_data(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  timeout = zx::deadline_after(zx::msec(1000));
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 1 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 0) {
    FDF_LOG(WARNING, "Write cap data reset for addr %x timed out", addr);
  }

  phy3_r4.set_phy_cr_write(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  phy3_r4.set_phy_cr_write(1);
  phy3_r4.WriteTo(usbphy3_mmio);
  timeout = zx::deadline_after(zx::msec(1000));
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 0 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 1) {
    FDF_LOG(WARNING, "Write for addr %x timed out", addr);
  }

  phy3_r4.set_phy_cr_write(0);
  phy3_r4.WriteTo(usbphy3_mmio);
  timeout = zx::deadline_after(zx::msec(1000));
  do {
    phy3_r5 = PHY3_R5::Get().ReadFrom(usbphy3_mmio);
  } while (phy3_r5.phy_cr_ack() == 1 && timeout > zx::clock::get_monotonic());

  if (phy3_r5.phy_cr_ack() != 0) {
    FDF_LOG(WARNING, "Disable write for addr %x timed out", addr);
  }

  return ZX_OK;
}

zx_status_t Vim3UsbPhy::InitPhy() {
  auto* usbctrl_mmio = &usbctrl_mmio_;

  // first reset USB
  int portnum = USB2PHY_PORTCOUNT;
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
    auto u2p_ro_v2 = U2P_R0_V2::Get(phy.idx()).ReadFrom(usbctrl_mmio).set_por(1);
    if (phy.is_otg_capable()) {
      u2p_ro_v2.set_idpullup0(1)
          .set_drvvbus0(1)
          .set_host_device(phy.dr_mode() == USB_MODE_PERIPHERAL ? 0 : 1)
          .WriteTo(usbctrl_mmio);
    } else {
      u2p_ro_v2.set_host_device(phy.dr_mode() == USB_MODE_HOST).WriteTo(usbctrl_mmio);
    }
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
    USB_PHY_REG21::Get().ReadFrom(mmio).set_usb2_otg_aca_en(0).WriteTo(mmio);

    auto u2p_r1 = U2P_R1_V2::Get(phy.idx());
    int count = 0;
    while (!u2p_r1.ReadFrom(usbctrl_mmio).phy_rdy()) {
      // wait phy ready max 5ms, common is 100us
      if (count > 1000) {
        FDF_LOG(WARNING, "Vim3UsbPhy::InitPhy U2P_R1_PHY_RDY wait failed");
        break;
      }

      count++;
      zx::nanosleep(zx::deadline_after(zx::usec(5)));
    }
  }

  // One time PLL initialization
  for (auto& phy : usbphy2_) {
    InitPll(&phy.mmio());
  }

  return ZX_OK;
}

zx_status_t Vim3UsbPhy::InitPhy3() {
  auto* usbphy3_mmio = &usbphy3_mmio_;
  auto* usbctrl_mmio = &usbctrl_mmio_;

  auto reg = usbphy3_mmio->Read32(0);
  reg = reg | (3 << 5);
  usbphy3_mmio->Write32(reg, 0);

  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  USB_R3_V2::Get()
      .ReadFrom(usbctrl_mmio)
      .set_p30_ssc_en(1)
      .set_p30_ssc_range(2)
      .set_p30_ref_ssp_en(1)
      .WriteTo(usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(2)));

  USB_R2_V2::Get()
      .ReadFrom(usbctrl_mmio)
      .set_p30_pcs_tx_deemph_3p5db(0x15)
      .set_p30_pcs_tx_deemph_6db(0x20)
      .WriteTo(usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(2)));

  USB_R1_V2::Get()
      .ReadFrom(usbctrl_mmio)
      .set_u3h_host_port_power_control_present(1)
      .set_u3h_fladj_30mhz_reg(0x20)
      .set_p30_pcs_tx_swing_full(127)
      .WriteTo(usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(2)));

  auto phy3_r2 = PHY3_R2::Get().ReadFrom(usbphy3_mmio);
  phy3_r2.set_phy_tx_vboost_lvl(0x4);
  phy3_r2.WriteTo(usbphy3_mmio);
  zx::nanosleep(zx::deadline_after(zx::usec(2)));

  uint32_t data = CrBusRead(0x102d);
  data |= (1 << 7);
  CrBusWrite(0x102D, data);

  data = CrBusRead(0x1010);
  data &= ~0xff0;
  data |= 0x20;
  CrBusWrite(0x1010, data);

  data = CrBusRead(0x1006);
  data &= ~(1 << 6);
  data |= (1 << 7);
  data &= ~(0x7 << 8);
  data |= (0x3 << 8);
  data |= (0x1 << 11);
  CrBusWrite(0x1006, data);

  data = CrBusRead(0x1002);
  data &= ~0x3f80;
  data |= (0x16 << 7);
  data &= ~0x7f;
  data |= (0x7f | (1 << 14));
  CrBusWrite(0x1002, data);

  data = CrBusRead(0x30);
  data &= ~(0xf << 4);
  data |= (0x8 << 4);
  CrBusWrite(0x30, data);
  zx::nanosleep(zx::deadline_after(zx::usec(2)));

  auto phy3_r1 = PHY3_R1::Get().ReadFrom(usbphy3_mmio);
  phy3_r1.set_phy_los_bias(0x4);
  phy3_r1.set_phy_los_level(0x9);
  phy3_r1.WriteTo(usbphy3_mmio);

  return ZX_OK;
}

zx_status_t Vim3UsbPhy::InitOtg() {
  auto* mmio = &usbctrl_mmio_;

  USB_R1_V2::Get().ReadFrom(mmio).set_u3h_fladj_30mhz_reg(0x20).WriteTo(mmio);

  USB_R5_V2::Get().ReadFrom(mmio).set_iddig_en0(1).set_iddig_en1(1).set_iddig_th(255).WriteTo(mmio);

  return ZX_OK;
}

void Vim3UsbPhy::UsbPhy2::SetMode(UsbMode mode, Vim3UsbPhy* device) {
  ZX_DEBUG_ASSERT(mode == UsbMode::HOST || mode == UsbMode::PERIPHERAL);
  if ((dr_mode_ == USB_MODE_HOST && mode != UsbMode::HOST) ||
      (dr_mode_ == USB_MODE_PERIPHERAL && mode != UsbMode::PERIPHERAL)) {
    FDF_LOG(ERROR, "If dr_mode_ is not USB_MODE_OTG, dr_mode_ must match requested mode.");
    return;
  }

  FDF_LOG(INFO, "Entering USB %s Mode", mode == UsbMode::HOST ? "Host" : "Peripheral");

  if (mode == phy_mode_)
    return;

  if (is_otg_capable_) {
    auto r0 = USB_R0_V2::Get().ReadFrom(&device->usbctrl_mmio_);
    if (mode == UsbMode::HOST) {
      r0.set_u2d_act(0);
    } else {
      r0.set_u2d_act(1);
      r0.set_u2d_ss_scaledown_mode(0);
    }
    r0.WriteTo(&device->usbctrl_mmio_);

    USB_R4_V2::Get()
        .ReadFrom(&device->usbctrl_mmio_)
        .set_p21_sleepm0(mode == UsbMode::PERIPHERAL)
        .WriteTo(&device->usbctrl_mmio_);
  }

  U2P_R0_V2::Get(idx_)
      .ReadFrom(&device->usbctrl_mmio_)
      .set_host_device(mode == UsbMode::HOST)
      .set_por(0)
      .WriteTo(&device->usbctrl_mmio_);

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  auto old_mode = phy_mode_;
  phy_mode_ = mode;

  if (is_otg_capable_ && old_mode != UsbMode::UNKNOWN) {
    PLL_REGISTER::Get(0x38)
        .FromValue(mode == UsbMode::HOST ? device->pll_settings_[6] : 0)
        .WriteTo(&mmio_);
    PLL_REGISTER::Get(0x34).FromValue(device->pll_settings_[5]).WriteTo(&mmio_);
  }
}

void Vim3UsbPhy::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                           const zx_packet_interrupt_t* interrupt) {
  if (status == ZX_ERR_CANCELED) {
    return;
  }
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "irq_.wait failed: %d", status);
    return;
  }

  {
    fbl::AutoLock _(&lock_);
    auto r5 = USB_R5_V2::Get().ReadFrom(&usbctrl_mmio_);
    // Acknowledge interrupt
    r5.set_usb_iddig_irq(0).WriteTo(&usbctrl_mmio_);

    // Read current host/device role.
    for (auto& phy : usbphy2_) {
      if (phy.dr_mode() != USB_MODE_OTG) {
        continue;
      }

      auto mode = r5.iddig_curr() == 0 ? UsbMode::HOST : UsbMode::PERIPHERAL;
      phy.SetMode(mode, this);

      if (mode == UsbMode::HOST) {
        auto result = controller_->AddXhci();
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to add xHCI, %s", result.status_string());
        }
        result = controller_->RemoveDwc2();
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to remove DWC2, %s", result.status_string());
        }
      } else {
        auto result = controller_->AddDwc2();
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to add DWC2, %s", result.status_string());
        }
        result = controller_->RemoveXhci();
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to remove xHCI, %s", result.status_string());
        }
      }
    }
  }

  irq_.ack();
}

zx::result<> Vim3UsbPhyDevice::Start() {
  // Get Reset Register.
  fidl::ClientEnd<fuchsia_hardware_registers::Device> reset_register;
  {
    zx::result result =
        incoming()->Connect<fuchsia_hardware_registers::Service::Device>("register-reset");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open i2c service: %s", result.status_string());
      return result.take_error();
    }
    reset_register = std::move(result.value());
  }

  // Get metadata.
  PhyMetadata parsed_metadata;
  {
    zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>("pdev");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open compat service: %s", result.status_string());
      return result.take_error();
    }
    auto compat = fidl::WireSyncClient(std::move(result.value()));
    if (!compat.is_valid()) {
      FDF_LOG(ERROR, "Failed to get compat");
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto metadata = compat->GetMetadata();
    if (!metadata.ok()) {
      FDF_LOG(ERROR, "Failed to GetMetadata %s", metadata.error().FormatDescription().c_str());
      return zx::error(metadata.error().status());
    }
    if (metadata->is_error()) {
      FDF_LOG(ERROR, "Failed to GetMetadata %s", zx_status_get_string(metadata->error_value()));
      return metadata->take_error();
    }

    auto vals = ParseMetadata(metadata.value()->metadata);
    if (vals.is_error()) {
      FDF_LOG(ERROR, "Failed to ParseMetadata %s", zx_status_get_string(vals.error_value()));
      return vals.take_error();
    }
    parsed_metadata = vals.value();
  }

  // Get mmio.
  std::optional<fdf::MmioBuffer> usbctrl_mmio, usbphy20_mmio, usbphy21_mmio, usbphy3_mmio;
  zx::interrupt irq;
  {
    zx::result result =
        incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open pdev service: %s", result.status_string());
      return result.take_error();
    }
    auto pdev = ddk::PDevFidl(std::move(result.value()));
    if (!pdev.is_valid()) {
      FDF_LOG(ERROR, "Failed to get pdev");
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto status = pdev.MapMmio(0, &usbctrl_mmio);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "pdev.MapMmio(0) error %s", zx_status_get_string(status));
      return zx::error(status);
    }
    status = pdev.MapMmio(1, &usbphy20_mmio);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "pdev.MapMmio(1) error %s", zx_status_get_string(status));
      return zx::error(status);
    }
    status = pdev.MapMmio(2, &usbphy21_mmio);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "pdev.MapMmio(2) error %s", zx_status_get_string(status));
      return zx::error(status);
    }
    status = pdev.MapMmio(3, &usbphy3_mmio);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "pdev.MapMmio(3) error %s", zx_status_get_string(status));
      return zx::error(status);
    }

    status = pdev.GetInterrupt(0, &irq);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "pdev.GetInterrupt(0) error %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  // Create and initialize device
  device_ = std::make_unique<Vim3UsbPhy>(
      this, std::move(reset_register), parsed_metadata.pll_settings, std::move(*usbctrl_mmio),
      Vim3UsbPhy::UsbPhy2(0, std::move(*usbphy20_mmio), false, USB_MODE_HOST),
      Vim3UsbPhy::UsbPhy2(1, std::move(*usbphy21_mmio), true, parsed_metadata.dr_mode),
      std::move(*usbphy3_mmio), std::move(irq));

  // Serve fuchsia_hardware_usb_phy.
  {
    auto result = outgoing()->AddService<fuchsia_hardware_usb_phy::Service>(
        fuchsia_hardware_usb_phy::Service::InstanceHandler({
            .device = bindings_.CreateHandler(device_.get(), fdf::Dispatcher::GetCurrent()->get(),
                                              fidl::kIgnoreBindingClosure),
        }));
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
      return zx::error(result.status_value());
    }
  }

  {
    auto result = CreateNode();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create node %s", result.status_string());
      return zx::error(result.status_value());
    }
  }

  // Initialize device. Must come after CreateNode() because Init() will creates xHCI and DWC2
  // nodes on top of node_.
  auto status = device_->Init();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Init() error %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> Vim3UsbPhyDevice::CreateNode() {
  // Add node for vim3-usb-phy.
  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, kDeviceName).Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create controller endpoints: %s",
                controller_endpoints.status_string());
  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create node endpoints: %s",
                node_endpoints.status_string());

  {
    fidl::WireResult result = fidl::WireCall(node())->AddChild(
        args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add child %s", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
  }
  controller_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));

  return zx::ok();
}

zx::result<> Vim3UsbPhyDevice::AddDevice(ChildNode& node) {
  fbl::AutoLock _(&node.lock_);
  node.count_++;
  if (node.count_ != 1) {
    return zx::ok();
  }

  {
    auto result = node.compat_server_.Initialize(incoming(), outgoing(), node_name(), node.name_,
                                                 compat::ForwardMetadata::None(), std::nullopt,
                                                 std::string(kDeviceName) + "/");
    if (result.is_error()) {
      return result.take_error();
    }
  }

  fidl::Arena arena;
  auto offers = node.compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_usb_phy::Service>(arena, node.name_));
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
          .name(arena, node.name_)
          .offers2(arena, std::move(offers))
          .properties(arena,
                      std::vector{
                          fdf::MakeProperty(arena, BIND_PLATFORM_DEV_VID, PDEV_VID_GENERIC),
                          fdf::MakeProperty(arena, BIND_PLATFORM_DEV_PID, PDEV_PID_GENERIC),
                          fdf::MakeProperty(arena, BIND_PLATFORM_DEV_DID, node.property_did_),
                      })
          .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create controller endpoints: %s",
                controller_endpoints.status_string());

  fidl::WireResult result = node_->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.FormatDescription().c_str());
    return zx::error(result.status());
  }
  node.controller_.Bind(std::move(controller_endpoints->client));

  return zx::ok();
}

zx::result<> Vim3UsbPhyDevice::RemoveDevice(ChildNode& node) {
  fbl::AutoLock _(&node.lock_);
  if (node.count_ == 0) {
    // Nothing to remove.
    return zx::ok();
  }
  node.count_--;
  if (node.count_ != 0) {
    // Has more instances.
    return zx::ok();
  }

  node.reset();
  return zx::ok();
}

zx_status_t Vim3UsbPhy::Init() {
  auto status = InitPhy();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "InitPhy() error %s", zx_status_get_string(status));
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

  bool has_otg = false;
  for (auto& phy : usbphy2_) {
    UsbMode mode;
    if (phy.dr_mode() != USB_MODE_OTG) {
      mode = phy.dr_mode() == USB_MODE_HOST ? UsbMode::HOST : UsbMode::PERIPHERAL;
      lock_.Acquire();
    } else {
      has_otg = true;
      // Wait for PHY to stabilize before reading initial mode.
      zx::nanosleep(zx::deadline_after(zx::sec(1)));
      lock_.Acquire();
      mode = USB_R5_V2::Get().ReadFrom(&usbctrl_mmio_).iddig_curr() == 0 ? UsbMode::HOST
                                                                         : UsbMode::PERIPHERAL;
    }
    phy.SetMode(mode, this);
    lock_.Release();

    if (mode == UsbMode::HOST) {
      auto result = controller_->AddXhci();
      if (result.is_error()) {
        FDF_LOG(ERROR, "Failed to add xHCI, %s", result.status_string());
      }
    } else {
      auto result = controller_->AddDwc2();
      if (result.is_error()) {
        FDF_LOG(ERROR, "Failed to add DWC2, %s", result.status_string());
      }
    }
  }

  // USB 3.0 phy is host mode only.
  {
    auto result = controller_->AddXhci();
    if (result.is_error()) {
      return result.status_value();
    }
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
void Vim3UsbPhy::ConnectStatusChanged(ConnectStatusChangedRequest& request,
                                      ConnectStatusChangedCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);

  if (dwc2_connected_ == request.connected())
    return;

  for (auto& phy : usbphy2_) {
    if (phy.mode() != UsbMode::PERIPHERAL) {
      continue;
    }
    auto* mmio = &phy.mmio();

    if (request.connected()) {
      PLL_REGISTER::Get(0x38).FromValue(pll_settings_[7]).WriteTo(mmio);
      PLL_REGISTER::Get(0x34).FromValue(pll_settings_[5]).WriteTo(mmio);
    } else {
      InitPll(mmio);
    }
  }

  dwc2_connected_ = request.connected();
}

void Vim3UsbPhyDevice::Stop() {
  auto status = controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove child: %s", status.status_string());
  }
}

}  // namespace vim3_usb_phy

FUCHSIA_DRIVER_EXPORT(vim3_usb_phy::Vim3UsbPhyDevice);
