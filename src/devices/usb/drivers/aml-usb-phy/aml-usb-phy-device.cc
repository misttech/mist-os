// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy-device.h"

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/ddk/binding_priv.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <fbl/auto_lock.h>

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy.h"
#include "src/devices/usb/drivers/aml-usb-phy/power-regs.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-regs.h"

namespace aml_usb_phy {

namespace {

[[maybe_unused]] void dump_power_regs(const fdf::MmioBuffer& mmio) {
  DUMP_REG(A0_RTI_GEN_PWR_SLEEP0, mmio)
  DUMP_REG(A0_RTI_GEN_PWR_ISO0, mmio)
}

[[maybe_unused]] void dump_hhi_mem_pd_regs(const fdf::MmioBuffer& mmio) {
  DUMP_REG(HHI_MEM_PD_REG0, mmio)
}

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
                    fdf::MmioBuffer& power_mmio, fdf::MmioBuffer& sleep_mmio,
                    bool dump_regs = false) {
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

  if (dump_regs) {
    dump_power_regs(sleep_mmio);
    dump_hhi_mem_pd_regs(power_mmio);
  }
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
  {
    zx::result pdev_result =
        incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
    if (pdev_result.is_error()) {
      FDF_LOG(ERROR, "Failed to open pdev service: %s", pdev_result.status_string());
      return pdev_result.take_error();
    }
    fidl::WireSyncClient pdev(std::move(pdev_result.value()));
    if (!pdev.is_valid()) {
      FDF_LOG(ERROR, "Failed to get pdev");
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    if (auto mmio = MapMmio(pdev, 0); mmio.is_error()) {
      return mmio.take_error();
    } else {
      usbctrl_mmio.emplace(*std::move(mmio));
    }

    uint32_t idx = 1;
    for (auto& phy_mode : parsed_metadata.phy_modes) {
      if (auto mmio = MapMmio(pdev, idx); mmio.is_error()) {
        return mmio.take_error();
      } else {
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
      }
      idx++;
    }

    if (auto result = pdev->GetInterruptById(0, 0); !result.ok()) {
      FDF_LOG(ERROR, "Call to GetInterruptbyId(0) failed %s", result.FormatDescription().c_str());
      return zx::error(result.status());
    } else if (result->is_error()) {
      FDF_LOG(ERROR, "GetInterruptbyId(0) failed %s", zx_status_get_string(result->error_value()));
      return result->take_error();
    } else {
      irq = std::move(result.value()->irq);
    }

    // Optional MMIOs
    {
      auto power_mmio = MapMmio(pdev, idx++);
      auto sleep_mmio = MapMmio(pdev, idx++);
      if (power_mmio.is_ok() && sleep_mmio.is_ok()) {
        FDF_LOG(INFO, "Found power and sleep MMIO.");
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
  auto status = device_->Init();
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

AmlUsbPhyDevice::ChildNode& AmlUsbPhyDevice::ChildNode::operator++() {
  fbl::AutoLock _(&lock_);
  count_++;
  if (count_ != 1) {
    return *this;
  }

  {
    auto result = compat_server_.Initialize(
        parent_->incoming(), parent_->outgoing(), parent_->node_name(), name_,
        compat::ForwardMetadata::None(), std::nullopt, std::string(kDeviceName) + "/");
    ZX_ASSERT_MSG(result.is_ok(), "Failed to initialize compat server: %s", result.status_string());
  }

  fidl::Arena arena;
  auto offers = compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_usb_phy::Service>(arena, name_));
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, name_)
                  .offers2(arena, std::move(offers))
                  .properties(arena,
                              std::vector{
                                  fdf::MakeProperty(arena, BIND_PLATFORM_DEV_VID, PDEV_VID_GENERIC),
                                  fdf::MakeProperty(arena, BIND_PLATFORM_DEV_PID, PDEV_PID_GENERIC),
                                  fdf::MakeProperty(arena, BIND_PLATFORM_DEV_DID, property_did_),
                              })
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create controller endpoints: %s",
                controller_endpoints.status_string());

  fidl::WireResult result =
      parent_->node_->AddChild(args, std::move(controller_endpoints->server), {});
  ZX_ASSERT_MSG(result.ok(), "Failed to add child %s", result.FormatDescription().c_str());
  ZX_ASSERT_MSG(result->is_ok(), "Failed to add child %d",
                static_cast<uint32_t>(result->error_value()));
  controller_.Bind(std::move(controller_endpoints->client));

  return *this;
}

AmlUsbPhyDevice::ChildNode& AmlUsbPhyDevice::ChildNode::operator--() {
  fbl::AutoLock _(&lock_);
  if (count_ == 0) {
    // Nothing to remove.
    return *this;
  }
  count_--;
  if (count_ != 0) {
    // Has more instances.
    return *this;
  }

  // Reset.
  if (controller_) {
    auto result = controller_->Remove();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to remove %s. %s", name_.data(), result.FormatDescription().c_str());
    }
    controller_.TakeClientEnd().reset();
  }
  compat_server_.reset();
  return *this;
}

void AmlUsbPhyDevice::Stop() {
  auto status = controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove child: %s", status.status_string());
  }
}

zx::result<fdf::MmioBuffer> AmlUsbPhyDevice::MapMmio(
    const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t idx) {
  auto mmio = pdev->GetMmioById(idx);
  if (!mmio.ok()) {
    FDF_LOG(ERROR, "Call to GetMmioById(%d) failed: %s", idx, mmio.FormatDescription().c_str());
    return zx::error(mmio.status());
  }
  if (mmio->is_error()) {
    FDF_LOG(ERROR, "GetMmioById(%d) failed: %s", idx, zx_status_get_string(mmio->error_value()));
    return mmio->take_error();
  }

  if (!mmio->value()->has_vmo() || !mmio->value()->has_size() || !mmio->value()->has_offset()) {
    FDF_LOG(ERROR, "GetMmioById(%d) returned invalid MMIO", idx);
    return zx::error(ZX_ERR_BAD_STATE);
  }

  zx::result mmio_buffer =
      fdf::MmioBuffer::Create(mmio->value()->offset(), mmio->value()->size(),
                              std::move(mmio->value()->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (mmio_buffer.is_error()) {
    FDF_LOG(ERROR, "Failed to map MMIO: %s", mmio_buffer.status_string());
    return zx::error(mmio_buffer.error_value());
  }

  return mmio_buffer.take_value();
}

}  // namespace aml_usb_phy

FUCHSIA_DRIVER_EXPORT(aml_usb_phy::AmlUsbPhyDevice);
