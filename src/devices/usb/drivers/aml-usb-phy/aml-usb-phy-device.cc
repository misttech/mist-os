// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy-device.h"

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/ddk/binding_priv.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <fbl/auto_lock.h>

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-regs.h"

namespace aml_usb_phy {

namespace {

struct PhyMetadata {
  std::array<uint32_t, 8> pll_settings;
  PhyType type;
  std::vector<UsbPhyMode> phy_modes;
};

zx::result<PhyMetadata> ParseMetadata(
    const fidl::VectorView<fuchsia_driver_compat::wire::Metadata>& metadata) {
  PhyMetadata parsed_metadata;
  bool found_pll_settings = false;
  bool found_phy_type = false;
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

    if (m.type == (DEVICE_METADATA_PRIVATE_PHY_TYPE | DEVICE_METADATA_PRIVATE)) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size %s", zx_status_get_string(status));
        continue;
      }

      if (size != sizeof(PhyType)) {
        FDF_LOG(ERROR, "Unexpected metadata size: got %zu, expected %zu", size, sizeof(PhyType));
        continue;
      }

      status = m.data.read(&parsed_metadata.type, 0, size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read %s", zx_status_get_string(status));
        continue;
      }

      found_phy_type = true;
    }

    if (m.type == DEVICE_METADATA_USB_MODE) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size %s", zx_status_get_string(status));
        continue;
      }

      if (size % sizeof(UsbPhyMode)) {
        FDF_LOG(ERROR, "Unexpected metadata size: got %zu, expected divisible by %zu", size,
                sizeof(UsbPhyMode));
        continue;
      }

      parsed_metadata.phy_modes.resize(size / sizeof(UsbPhyMode));
      status = m.data.read(parsed_metadata.phy_modes.data(), 0, size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read %s", zx_status_get_string(status));
        continue;
      }
    }
  }

  if (found_pll_settings && found_phy_type) {
    return zx::ok(parsed_metadata);
  }

  FDF_LOG(ERROR, "Failed to parse metadata. Metadata needs to have pll_settings and phy_type.");
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx_status_t PowerOn(fidl::ClientEnd<fuchsia_hardware_registers::Device>& reset_register,
                    fdf::MmioBuffer& power_mmio, fdf::MmioBuffer& sleep_mmio) {
  A0_RTI_GEN_PWR_SLEEP0::Get().ReadFrom(&sleep_mmio).set_usb_comb_power_off(0).WriteTo(&sleep_mmio);
  HHI_MEM_PD_REG0::Get().ReadFrom(&power_mmio).set_usb_comb_pd(0).WriteTo(&power_mmio);
  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  fidl::Arena<> arena;
  auto register_result1 =
      fidl::WireCall(reset_register).buffer(arena)->WriteRegister32(RESET1_LEVEL_OFFSET, 0x4, 0);
  if ((register_result1.status() != ZX_OK) || register_result1->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 2 failed\n");
    return ZX_ERR_INTERNAL;
  }
  zx::nanosleep(zx::deadline_after(zx::usec(100)));
  A0_RTI_GEN_PWR_ISO0::Get()
      .ReadFrom(&sleep_mmio)
      .set_usb_comb_isolation_enable(0)
      .WriteTo(&sleep_mmio);

  auto register_result2 =
      fidl::WireCall(reset_register).buffer(arena)->WriteRegister32(RESET1_LEVEL_OFFSET, 0x4, 0x4);
  if ((register_result2.status() != ZX_OK) || register_result2->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 2 failed\n");
    return ZX_ERR_INTERNAL;
  }
  zx::nanosleep(zx::deadline_after(zx::usec(100)));
  A0_RTI_GEN_PWR_SLEEP0::Get().ReadFrom(&sleep_mmio).set_pci_comb_power_off(0).WriteTo(&sleep_mmio);

  auto register_result3 = fidl::WireCall(reset_register)
                              .buffer(arena)
                              ->WriteRegister32(RESET1_LEVEL_OFFSET, 0xF << 26, 0);
  if ((register_result3.status() != ZX_OK) || register_result3->is_error()) {
    FDF_LOG(ERROR, "Reset Register Write on 1 << 2 failed\n");
    return ZX_ERR_INTERNAL;
  }

  A0_RTI_GEN_PWR_ISO0::Get()
      .ReadFrom(&sleep_mmio)
      .set_pci_comb_isolation_enable(0)
      .WriteTo(&sleep_mmio);
  A0_RTI_GEN_PWR_SLEEP0::Get().ReadFrom(&sleep_mmio).set_ge2d_power_off(0).WriteTo(&sleep_mmio);

  HHI_MEM_PD_REG0::Get().ReadFrom(&power_mmio).set_ge2d_pd(0).WriteTo(&power_mmio);

  A0_RTI_GEN_PWR_ISO0::Get()
      .ReadFrom(&sleep_mmio)
      .set_ge2d_isolation_enable(0)
      .WriteTo(&sleep_mmio);
  A0_RTI_GEN_PWR_ISO0::Get()
      .ReadFrom(&sleep_mmio)
      .set_ge2d_isolation_enable(1)
      .WriteTo(&sleep_mmio);

  HHI_MEM_PD_REG0::Get().ReadFrom(&power_mmio).set_ge2d_pd(0xFF).WriteTo(&power_mmio);
  A0_RTI_GEN_PWR_SLEEP0::Get().ReadFrom(&sleep_mmio).set_ge2d_power_off(1).WriteTo(&sleep_mmio);
  return ZX_OK;
}

}  // namespace

zx::result<> AmlUsbPhyDevice::Start() {
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
  std::optional<fdf::MmioBuffer> usbctrl_mmio;
  std::vector<UsbPhy2> usbphy2;
  std::vector<UsbPhy3> usbphy3;
  zx::interrupt irq;
  bool has_otg = false;
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
    uint32_t idx = 1;
    for (auto& phy_mode : parsed_metadata.phy_modes) {
      std::optional<fdf::MmioBuffer> mmio;
      status = pdev.MapMmio(idx, &mmio);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "pdev.MapMmio(%d) error %s", idx, zx_status_get_string(status));
        return zx::error(status);
      }
      switch (phy_mode.protocol) {
        case Usb2_0: {
          usbphy2.emplace_back(usbphy2.size(), std::move(*mmio), phy_mode.is_otg_capable,
                               phy_mode.dr_mode);
        } break;
        case Usb3_0: {
          usbphy3.emplace_back(std::move(*mmio), phy_mode.is_otg_capable, phy_mode.dr_mode);
        } break;
        default:
          FDF_LOG(ERROR, "Unsupported protocol type %d", phy_mode.protocol);
          break;
      }
      idx++;
      if (phy_mode.dr_mode == USB_MODE_OTG) {
        has_otg = true;
      }
    }

    status = pdev.GetInterrupt(0, &irq);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "pdev.GetInterrupt(0) error %s", zx_status_get_string(status));
      return zx::error(status);
    }

    // Optional MMIOs
    {
      std::optional<fdf::MmioBuffer> power_mmio, sleep_mmio;
      auto status = pdev.MapMmio(idx++, &power_mmio);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "pdev.MapMmio(%d) error %s", idx - 1, zx_status_get_string(status));
      }
      status = pdev.MapMmio(idx++, &sleep_mmio);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "pdev.MapMmio(%d) error %s", idx - 1, zx_status_get_string(status));
      }

      if (power_mmio.has_value() && sleep_mmio.has_value()) {
        auto status = PowerOn(reset_register, *power_mmio, *sleep_mmio);
        if (status != ZX_OK) {
          FDF_LOG(ERROR, "PowerOn() error %s", zx_status_get_string(status));
          return zx::error(status);
        }
      }
    }
  }

  // Create and initialize device
  device_ = std::make_unique<AmlUsbPhy>(this, parsed_metadata.type, std::move(reset_register),
                                        parsed_metadata.pll_settings, std::move(*usbctrl_mmio),
                                        std::move(irq), std::move(usbphy2), std::move(usbphy3));

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

  // Initialize device. Must come after CreateNode() because Init() will create xHCI and DWC2
  // nodes on top of node_.
  auto status = device_->Init(has_otg);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Init() error %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> AmlUsbPhyDevice::CreateNode() {
  // Add node for aml-usb-phy.
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

zx::result<> AmlUsbPhyDevice::AddDevice(ChildNode& node) {
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

zx::result<> AmlUsbPhyDevice::RemoveDevice(ChildNode& node) {
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

void AmlUsbPhyDevice::Stop() {
  auto status = controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove child: %s", status.status_string());
  }
}

}  // namespace aml_usb_phy

FUCHSIA_DRIVER_EXPORT(aml_usb_phy::AmlUsbPhyDevice);
