// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy-device.h"

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <mutex>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy.h"
#include "src/devices/usb/drivers/aml-usb-phy/power-regs.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-regs.h"

namespace aml_usb_phy {

namespace {

[[maybe_unused]] void dump_power_regs(const fdf::MmioBuffer& mmio) {
  DUMP_REG(A0_RTI_GEN_PWR_SLEEP0, mmio)
  DUMP_REG(A0_RTI_GEN_PWR_ISO0, mmio)
}

[[maybe_unused]] void dump_hhi_mem_pd_regs(const fdf::MmioBuffer& mmio){
    DUMP_REG(HHI_MEM_PD_REG0, mmio)}

zx_status_t
    PowerOn(fidl::ClientEnd<fuchsia_hardware_registers::Device>& reset_register,
            fdf::MmioBuffer& power_mmio, fdf::MmioBuffer& sleep_mmio, bool dump_regs = false) {
  A0_RTI_GEN_PWR_SLEEP0::Get().ReadFrom(&sleep_mmio).set_usb_comb_power_off(0).WriteTo(&sleep_mmio);
  HHI_MEM_PD_REG0::Get().ReadFrom(&power_mmio).set_usb_comb_pd(0).WriteTo(&power_mmio);
  zx::nanosleep(zx::deadline_after(zx::usec(100)));

  fidl::Arena<> arena;
  fidl::WireUnownedResult register_result1 =
      fidl::WireCall(reset_register).buffer(arena)->WriteRegister32(RESET1_LEVEL_OFFSET, 0x4, 0);
  if (!register_result1.ok() || register_result1->is_error()) {
    fdf::error("Reset Register Write on 1 << 2 failed: {}",
               register_result1.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }
  zx::nanosleep(zx::deadline_after(zx::usec(100)));
  A0_RTI_GEN_PWR_ISO0::Get()
      .ReadFrom(&sleep_mmio)
      .set_usb_comb_isolation_enable(0)
      .WriteTo(&sleep_mmio);

  fidl::WireUnownedResult register_result2 =
      fidl::WireCall(reset_register).buffer(arena)->WriteRegister32(RESET1_LEVEL_OFFSET, 0x4, 0x4);
  if (!register_result2.ok() || register_result2->is_error()) {
    fdf::error("Reset Register Write on 1 << 2 failedd: {}",
               register_result2.FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }
  zx::nanosleep(zx::deadline_after(zx::usec(100)));
  A0_RTI_GEN_PWR_SLEEP0::Get().ReadFrom(&sleep_mmio).set_pci_comb_power_off(0).WriteTo(&sleep_mmio);

  fidl::WireUnownedResult register_result3 =
      fidl::WireCall(reset_register)
          .buffer(arena)
          ->WriteRegister32(RESET1_LEVEL_OFFSET, 0xF << 26, 0);
  if (!register_result3.ok() || register_result3->is_error()) {
    fdf::error("Reset Register Write on 1 << 2 failed: {}",
               register_result3.FormatDescription().c_str());
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
      fdf::error("Failed to open i2c service: {}", result);
      return result.take_error();
    }
    reset_register = std::move(result.value());
  }

  // Get metadata.
  zx::result usb_phy_metadata = compat::GetMetadata<fuchsia_hardware_usb_phy::Metadata>(
      incoming(), DEVICE_METADATA_USB_MODE, "pdev");
  if (usb_phy_metadata.is_error()) {
    fdf::error("Failed to get metadata: {}", usb_phy_metadata);
    return usb_phy_metadata.take_error();
  }

  // Get mmio.
  std::optional<fdf::MmioBuffer> usbctrl_mmio;
  std::vector<UsbPhy2> usbphy2;
  std::vector<UsbPhy3> usbphy3;
  zx::interrupt irq;
  bool needs_hack = false;
  {
    zx::result pdev_client_end =
        incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
    if (pdev_client_end.is_error()) {
      fdf::error("Failed to connect to platform device: {}", pdev_client_end);
      return pdev_client_end.take_error();
    }
    fdf::PDev pdev{std::move(pdev_client_end.value())};

    zx::result dev_info = pdev.GetDeviceInfo();
    if (dev_info.is_ok() &&
        (dev_info->pid == bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_S905D2 ||
         dev_info->pid == bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_S905D3)) {
      fdf::error("Using hack");
      needs_hack = true;
    }

    zx::result mmio = MapMmio(pdev, 0);
    if (mmio.is_error()) {
      fdf::error("Failed to map mmio: {}", mmio);
      return mmio.take_error();
    }
    usbctrl_mmio.emplace(*std::move(mmio));

    uint32_t idx = 1;
    const auto& usb_phy_modes = usb_phy_metadata.value().usb_phy_modes();
    if (!usb_phy_modes.has_value()) {
      fdf::error("Metadata missing usb_phy_modes field");
      return zx::error(ZX_ERR_INTERNAL);
    }
    for (size_t i = 0; i < usb_phy_modes.value().size(); ++i) {
      zx::result mmio = MapMmio(pdev, idx);
      if (mmio.is_error()) {
        return mmio.take_error();
      }
      const auto& phy_mode = usb_phy_modes.value()[i];
      const auto& protocol = phy_mode.protocol();
      if (!protocol.has_value()) {
        fdf::error("Phy-mode {} missing protocol field", i);
        return zx::error(ZX_ERR_INTERNAL);
      }
      const auto& is_otg_capable = phy_mode.is_otg_capable();
      if (!is_otg_capable.has_value()) {
        fdf::error("Phy-mode {} missing is_otg_capable field", i);
        return zx::error(ZX_ERR_INTERNAL);
      }
      const auto& dr_mode = phy_mode.dr_mode();
      if (!is_otg_capable.has_value()) {
        fdf::error("Phy-mode {} missing dr_mode field", i);
        return zx::error(ZX_ERR_INTERNAL);
      }

      switch (protocol.value()) {
        case fuchsia_hardware_usb_phy::ProtocolVersion::kUsb20: {
          usbphy2.emplace_back(usbphy2.size(), std::move(*mmio), is_otg_capable.value(),
                               dr_mode.value());
        } break;
        case fuchsia_hardware_usb_phy::ProtocolVersion::kUsb30: {
          usbphy3.emplace_back(std::move(*mmio), is_otg_capable.value(), dr_mode.value());
        } break;
        default:
          fdf::error("Unsupported protocol type {}", static_cast<uint32_t>(protocol.value()));
          break;
      }
      idx++;
    }

    zx::result irq_result = pdev.GetInterrupt(0);
    if (irq_result.is_error()) {
      fdf::error("Failed to get interrupt: {}", irq_result);
      return irq_result.take_error();
    }
    irq = std::move(irq_result.value());

    // Optional MMIOs
    {
      auto power_mmio = MapMmio(pdev, idx++);
      auto sleep_mmio = MapMmio(pdev, idx++);
      if (power_mmio.is_ok() && sleep_mmio.is_ok()) {
        fdf::info("Found power and sleep MMIO.");
        auto status = PowerOn(reset_register, *power_mmio, *sleep_mmio);
        if (status != ZX_OK) {
          fdf::error("Failed to power on: {}", zx_status_get_string(status));
          return zx::error(status);
        }
      }
    }
  }

  // Create and initialize device
  const auto& phy_type = usb_phy_metadata.value().phy_type();
  if (!phy_type.has_value()) {
    fdf::error("Metadata missing phy_type field");
    return zx::error(ZX_ERR_INTERNAL);
  }
  device_ = std::make_unique<AmlUsbPhy>(this, phy_type.value(), std::move(reset_register),
                                        std::move(*usbctrl_mmio), std::move(irq),
                                        std::move(usbphy2), std::move(usbphy3), needs_hack);

  {
    auto result = CreateNode();
    if (result.is_error()) {
      fdf::error("Failed to create node: {}", result);
      return zx::error(result.status_value());
    }
  }

  // Initialize device. Must come after CreateNode() because Init() will create xHCI and DWC2
  // nodes on top of node_.
  auto status = device_->Init();
  if (status != ZX_OK) {
    fdf::error("Init() error {}", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> AmlUsbPhyDevice::CreateNode() {
  // Add node for aml-usb-phy.
  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, kDeviceName).Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (node_endpoints.is_error()) {
    fdf::error("Failed to create node endpoints: {}", node_endpoints);
    return node_endpoints.take_error();
  }

  {
    fidl::WireResult result = fidl::WireCall(node())->AddChild(
        args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
    if (!result.ok()) {
      fdf::error("Failed to add child {}", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
  }
  controller_.Bind(std::move(controller_endpoints.client));
  node_.Bind(std::move(node_endpoints->client));

  return zx::ok();
}

AmlUsbPhyDevice::ChildNode& AmlUsbPhyDevice::ChildNode::operator++() {
  std::lock_guard<std::mutex> _(lock_);
  count_++;
  if (count_ != 1) {
    return *this;
  }

  // Serve fuchsia_hardware_usb_phy.
  {
    auto result = parent_->outgoing()->AddService<fuchsia_hardware_usb_phy::Service>(
        fuchsia_hardware_usb_phy::Service::InstanceHandler({
            .device = parent_->bindings_.CreateHandler(parent_->device_.get(),
                                                       fdf::Dispatcher::GetCurrent()->get(),
                                                       fidl::kIgnoreBindingClosure),
        }),
        name_);
    ZX_ASSERT_MSG(result.is_ok(), "Failed to add Device service: %s", result.status_string());
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
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
          .name(arena, name_)
          .offers2(arena, std::move(offers))
          .properties(arena,
                      std::vector{
                          fdf::MakeProperty(arena, bind_fuchsia::PLATFORM_DEV_VID,
                                            bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC),
                          fdf::MakeProperty(arena, bind_fuchsia::PLATFORM_DEV_PID,
                                            bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
                          fdf::MakeProperty(arena, bind_fuchsia::PLATFORM_DEV_DID, property_did_),
                      })
          .Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  fidl::WireResult result =
      parent_->node_->AddChild(args, std::move(controller_endpoints.server), {});
  ZX_ASSERT_MSG(result.ok(), "Failed to add child: %s", result.FormatDescription().c_str());
  ZX_ASSERT_MSG(result->is_ok(), "Failed to add child: %d",
                static_cast<uint32_t>(result->error_value()));
  controller_.Bind(std::move(controller_endpoints.client));

  return *this;
}

AmlUsbPhyDevice::ChildNode& AmlUsbPhyDevice::ChildNode::operator--() {
  std::lock_guard<std::mutex> _(lock_);
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
      fdf::error("Failed to remove {}. {}", name_.data(), result.FormatDescription().c_str());
    }
    controller_.TakeClientEnd().reset();
  }
  compat_server_.reset();
  {
    auto result = parent_->outgoing()->RemoveService<fuchsia_hardware_usb_phy::Service>(name_);
    if (result.is_error()) {
      fdf::error("Failed to remove Device service {}", result);
    }
  }
  return *this;
}

void AmlUsbPhyDevice::Stop() {
  auto status = controller_->Remove();
  if (!status.ok()) {
    fdf::error("Could not remove child: {}", status.status_string());
  }
}

zx::result<fdf::MmioBuffer> AmlUsbPhyDevice::MapMmio(fdf::PDev& pdev, uint32_t idx) {
  return pdev.MapMmio(idx);
}

}  // namespace aml_usb_phy

FUCHSIA_DRIVER_EXPORT(aml_usb_phy::AmlUsbPhyDevice);
