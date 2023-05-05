// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/a1-usb-phy/a1-usb-phy.h"

#include <assert.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/errors.h>

#include <cstdio>
#include <sstream>
#include <string>

#include <ddktl/fidl.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <soc/aml-common/aml-power-domain.h>

#include "src/devices/usb/drivers/a1-usb-phy/usb-phy-regs.h"

namespace {

constexpr uint8_t kPwrOn = 1;

// MMIO indices.
enum {
  MMIO_USB_CONTROL = 0,
  MMIO_USB_PHY = 1,
  MMIO_RESET = 2,
  MMIO_CLK = 3,
};

}  // namespace

namespace a1_usb_phy {

// Based on set_usb_pll() in phy-aml-new-usb2-v2.c
void A1UsbPhy::InitPll(fdf::MmioBuffer* mmio) {
  PLL_REGISTER_44::Get().FromValue(pll_settings_[1]).WriteTo(mmio);

  PLL_REGISTER_40::Get()
      .FromValue(0x0)
      .set_value(pll_settings_[0])
      .set_enable(0x1)
      .set_reset(0x1)
      .WriteTo(mmio);
  zx::nanosleep(zx::deadline_after(zx::usec(10)));

  PLL_REGISTER_48::Get().FromValue(pll_settings_[2]).WriteTo(mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(200)));

  PLL_REGISTER_40::Get()
      .FromValue(0x0)
      .set_value(pll_settings_[0])
      .set_enable(0x1)
      .set_reset(0x0)
      .WriteTo(mmio);
  zx::nanosleep(zx::deadline_after(zx::usec(10)));
  // PLL

  PLL_REGISTER_48::Get().FromValue(pll_settings_[2]).WriteTo(mmio);

  PLL_REGISTER_C::Get()
      .FromValue(0x0)
      .set_squelch_ref(0x4)
      .set_hsdic_ref(0x2)
      .set_TBD_4(0x1)
      .WriteTo(mmio);
  // Tuning

  PLL_REGISTER_50::Get().FromValue(pll_settings_[3]).WriteTo(mmio);
  PLL_REGISTER_54::Get()
      .FromValue(0x0)
      .set_usb2_cal_ack_en(0x1)
      .set_usb2_tx_strg_pd(0x1)
      .set_usb2_otg_aca_trim_1_0(0x2)
      .WriteTo(mmio);

  uint32_t temp = usbphy_mmio_->Read32(0x8);
  uint32_t temp1 = usbphy_mmio_->Read32(0x10);
  usbphy_mmio_->Write32((temp & 0xfff) | temp1, 0x10);

  PLL_REGISTER_34::Get()
      .FromValue(0x0)
      .set_Update_PMA_signals(0x1)
      .set_minimum_count_for_sync_detection(0x7)
      .WriteTo(mmio);

  PLL_REGISTER_38::Get()
      .FromValue(0x0)
      .set_bypass_ctrl_8_i_rpd_en(0x1)
      .set_bypass_ctrl_9_i_rpu_sw2_en(0x1)
      .WriteTo(mmio);
}

zx_status_t A1UsbPhy::InitPhy() {
  auto* usbctrl_mmio = &*usbctrl_mmio_;
  auto* reset_mmio = &*reset_mmio_;
  auto* usbphy_mmio = &*usbphy_mmio_;

  RESET_REGISTER::Get(RESET1_LEVEL_OFFSET).ReadFrom(reset_mmio).set_usbphy(0x1).WriteTo(reset_mmio);

  // amlogic_new_usbphy_reset_v2()
  RESET_REGISTER::Get(RESET1_REGISTER_OFFSET).FromValue(0x0).set_usbctrl(0x1).WriteTo(reset_mmio);

  // FIXME(voydanoff) this delay is very long, but it is what the Amlogic Linux kernel is doing.
  zx::nanosleep(zx::deadline_after(zx::usec(10)));

  U2P_R0_V2::Get(0).ReadFrom(usbctrl_mmio).set_por(0x1).WriteTo(usbctrl_mmio);
  U2P_R0_V2::Get(0).ReadFrom(usbctrl_mmio).set_host_device(0x1).WriteTo(usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(10)));

  RESET_REGISTER::Get(RESET1_LEVEL_OFFSET).ReadFrom(reset_mmio).set_usbphy(0x0).WriteTo(reset_mmio);
  zx::nanosleep(zx::deadline_after(zx::usec(200)));
  RESET_REGISTER::Get(RESET1_LEVEL_OFFSET).ReadFrom(reset_mmio).set_usbphy(0x1).WriteTo(reset_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(50)));
  PLL_REGISTER_54::Get().ReadFrom(usbphy_mmio).set_usb2_otg_aca_en(0x0).WriteTo(usbphy_mmio);

  auto u2p_r1 = U2P_R1_V2::Get(0);

  int count = 0;
  while (!u2p_r1.ReadFrom(usbctrl_mmio).phy_rdy()) {
    // wait phy ready max 5ms, common is 100us
    if (count > 1000) {
      zxlogf(ERROR, "A1UsbPhy::InitPhy U2P_R1_PHY_RDY wait failed");
      break;
    }

    count++;
    zx::nanosleep(zx::deadline_after(zx::usec(5)));
  }

  if (count > 1000) {
    return ZX_ERR_OUT_OF_RANGE;
  } else {
    return ZX_OK;
  }
}

zx_status_t A1UsbPhy::InitOtg() {
  auto* mmio = &*usbctrl_mmio_;

  USB_R1_V2::Get()
      .ReadFrom(mmio)
      .set_u3h_fladj_30mhz_reg(0x20)
      .set_u3h_host_u2_port_disable(0x2)
      .WriteTo(mmio);

  USB_R5_V2::Get().ReadFrom(mmio).set_iddig_en0(0x1).set_iddig_en1(0x1).set_iddig_th(0xff).WriteTo(
      mmio);

  return ZX_OK;
}

void A1UsbPhy::SetMode(UsbMode mode, SetModeCompletion completion) {
  ZX_DEBUG_ASSERT(mode == UsbMode::HOST || mode == UsbMode::PERIPHERAL);
  // Only the irq thread calls |SetMode|, and it should have waited for the
  // previous call to |SetMode| to complete.
  auto cleanup = fit::defer([&]() {
    if (completion)
      completion();
  });

  if (mode == phy_mode_)
    return;

  auto* usbctrl_mmio = &*usbctrl_mmio_;

  auto r0 = USB_R0_V2::Get().ReadFrom(usbctrl_mmio);
  if (mode == UsbMode::HOST) {
    r0.set_u2d_act(0x0);
  } else {
    r0.set_u2d_act(0x1);
    r0.set_u2d_ss_scaledown_mode(0x0);
  }
  r0.WriteTo(usbctrl_mmio);

  USB_R4_V2::Get()
      .ReadFrom(usbctrl_mmio)
      .set_p21_sleepm0(mode == UsbMode::PERIPHERAL)
      .WriteTo(usbctrl_mmio);

  U2P_R0_V2::Get(0)
      .ReadFrom(usbctrl_mmio)
      .set_host_device(mode == UsbMode::HOST)
      .set_por(0x0)
      .WriteTo(usbctrl_mmio);

  zx::nanosleep(zx::deadline_after(zx::usec(500)));

  auto old_mode = phy_mode_;
  phy_mode_ = mode;

  if (old_mode == UsbMode::UNKNOWN) {
    // One time PLL initialization
    InitPll(&*usbphy_mmio_);
  } else {
    auto* phy_mmio = &*usbphy_mmio_;

    PLL_REGISTER_38::Get()
        .FromValue(mode == UsbMode::HOST ? pll_settings_[6] : 0)
        .WriteTo(phy_mmio);
    PLL_REGISTER_34::Get().FromValue(pll_settings_[5]).WriteTo(phy_mmio);
  }

  if (mode == UsbMode::HOST) {
    auto status = AddXhciDevice();
    if (status != ZX_OK) {
      zxlogf(ERROR, "AddXhciDevice() error %s", zx_status_get_string(status));
    }
  }
}

zx_status_t A1UsbPhy::Create(void* ctx, zx_device_t* parent) {
  auto dev = std::make_unique<A1UsbPhy>(parent);
  auto status = dev->Init();
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* _ = dev.release();
  return ZX_OK;
}

zx_status_t A1UsbPhy::UsbPowerOn() {
  static const zx_smc_parameters_t kSetPdCall = aml_pd_smc::CreatePdSmcCall(A1_PDID_USB, kPwrOn);

  if (!smc_short_circuit_) {
    ZX_ASSERT(smc_monitor_.is_valid());
    zx_smc_result_t result;
    auto status = zx_smc_call(smc_monitor_.get(), &kSetPdCall, &result);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Call zx_smc_call failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

void A1UsbPhy::InitUsbClk() {
  auto* clk_mmio = &*clk_mmio_;

  // clock source select: 0:cts_oscin_clk; 1:cts_sys_clk; 2:fclk_div3; 3:fclk_div5.
  USB_BUSCLK_CTRL::Get()
      .ReadFrom(clk_mmio)
      .set_clock_div(0x0)
      .set_clock_gate_en(0x1)
      .set_clock_source_select(0x1)
      .WriteTo(clk_mmio);
}

zx_status_t A1UsbPhy::AddXhciDevice() {
  if (xhci_device_) {
    zxlogf(ERROR, "Add xhci device repeatedly!");
    return ZX_ERR_BAD_STATE;
  }

  static zx_protocol_device_t ops = {
      .version = DEVICE_OPS_VERSION,
      // Defer get_protocol() to parent.
      .get_protocol =
          [](void* ctx, uint32_t id, void* proto) {
            return device_get_protocol(reinterpret_cast<A1UsbPhy*>(ctx)->zxdev(), id, proto);
          },
      .release = [](void*) {},
  };

  zx_device_prop_t props[] = {
      {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_GENERIC},
      {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_GENERIC},
      {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_USB_XHCI_COMPOSITE},
  };

  // clang-format off
  device_add_args_t args = (ddk::DeviceAddArgs("xhci")
                            .set_context(this)
                            .set_props(props)
                            .set_proto_id(ZX_PROTOCOL_USB_PHY)
                            .set_ops(&ops)).get();
  // clang-format on

  auto status = device_add(zxdev(), &args, &xhci_device_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_add() error %s", zx_status_get_string(status));
  }
  return status;
}

void A1UsbPhy::RemoveXhciDevice() {
  if (xhci_device_) {
    device_async_remove(xhci_device_);
    xhci_device_ = nullptr;
  }
}

zx_status_t A1UsbPhy::Init() {
  ddk::PDevFidl pdev(parent());
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "A1UsbPhy::Init: could not get platform device protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  size_t actual;
  zx_status_t status =
      DdkGetMetadata(DEVICE_METADATA_PRIVATE, pll_settings_, sizeof(pll_settings_), &actual);
  if (status != ZX_OK || actual != sizeof(pll_settings_)) {
    zxlogf(ERROR, "A1UsbPhy::Init could not get metadata for PLL settings");
    return ZX_ERR_INTERNAL;
  }
  status = DdkGetMetadata(DEVICE_METADATA_USB_MODE, &dr_mode_, sizeof(dr_mode_), &actual);
  if (status == ZX_OK && actual != sizeof(dr_mode_)) {
    zxlogf(ERROR, "A1UsbPhy::Init could not get metadata for USB Mode");
    return ZX_ERR_INTERNAL;
  } else if (status != ZX_OK) {
    dr_mode_ = USB_MODE_OTG;
  }

  // TODO(123426) Remove. See issue details.
  TestMetadata test_meta;
  status = DdkGetMetadata(DEVICE_METADATA_TEST, &test_meta, sizeof(test_meta), &actual);
  if (status == ZX_OK && actual == sizeof(test_meta)) {
    // Only supplied by tests, not otherwise present.
    smc_short_circuit_ = test_meta.smc_short_circuit;
  }

  status = pdev.MapMmio(MMIO_USB_CONTROL, &usbctrl_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio usbctrl failed %s", zx_status_get_string(status));
    return status;
  }

  status = pdev.MapMmio(MMIO_USB_PHY, &usbphy_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio usbphy failed %s", zx_status_get_string(status));
    return status;
  }

  status = pdev.MapMmio(MMIO_RESET, &reset_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio reset failed %s", zx_status_get_string(status));
    return status;
  }

  status = pdev.MapMmio(MMIO_CLK, &clk_mmio_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio clk failed %s", zx_status_get_string(status));
    return status;
  }

  if (!smc_short_circuit_) {
    // TODO(123426) Can't (yet) use fake smc resources. See issue details.
    status = pdev.GetSmc(0, &smc_monitor_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "unable to sip monitor handle %s", zx_status_get_string(status));
      return status;
    }
  }

  // USB module power supply.
  status = UsbPowerOn();
  if (status != ZX_OK) {
    zxlogf(ERROR, "USB Power Domain Control failed %s", zx_status_get_string(status));
    return status;
  }

  // USB bus clock configuration.
  InitUsbClk();

  status = InitPhy();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Usb phy initialization failed %s", zx_status_get_string(status));
    return status;
  }
  status = InitOtg();
  if (status != ZX_OK) {
    return status;
  }

  return DdkAdd("a1-usb-phy", DEVICE_ADD_NON_BINDABLE);
}

void A1UsbPhy::DdkInit(ddk::InitTxn txn) {
  if (dr_mode_ != USB_MODE_OTG) {
    sync_completion_t set_mode_sync;
    auto completion = [&](void) { sync_completion_signal(&set_mode_sync); };
    fbl::AutoLock lock(&lock_);
    if (dr_mode_ == USB_MODE_PERIPHERAL) {
      zxlogf(INFO, "Entering USB Peripheral Mode");
      SetMode(UsbMode::PERIPHERAL, std::move(completion));
    } else {
      zxlogf(INFO, "Entering USB Host Mode");
      SetMode(UsbMode::HOST, std::move(completion));
    }
    sync_completion_wait(&set_mode_sync, ZX_TIME_INFINITE);

    return txn.Reply(ZX_OK);
  }

  // If dr_mode_ is USB_MODE_OTG, the controller does not support OTG negotiation, and an error is
  // reported.
  zxlogf(ERROR, "controller does not support OTG mode");
  return txn.Reply(ZX_ERR_NOT_SUPPORTED);
}

// PHY tuning based on connection state
void A1UsbPhy::UsbPhyConnectStatusChanged(bool connected) {
  fbl::AutoLock lock(&lock_);

  if (dwc2_connected_ == connected)
    return;

  auto* mmio = &*usbphy_mmio_;

  if (connected) {
    PLL_REGISTER_38::Get().FromValue(pll_settings_[7]).WriteTo(mmio);
    PLL_REGISTER_34::Get().FromValue(pll_settings_[5]).WriteTo(mmio);
  } else {
    InitPll(mmio);
  }

  dwc2_connected_ = connected;
}

void A1UsbPhy::DdkUnbind(ddk::UnbindTxn txn) {
  fbl::AutoLock lock(&lock_);
  RemoveXhciDevice();
  txn.Reply();
}

void A1UsbPhy::DdkRelease() { delete this; }

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = A1UsbPhy::Create;
  return ops;
}();

}  // namespace a1_usb_phy

ZIRCON_DRIVER(a1_usb_phy, a1_usb_phy::driver_ops, "zircon", "0.1");
