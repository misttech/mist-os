// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <fidl/fuchsia.hardware.pci/cpp/common_types.h>
#include <fidl/fuchsia.hardware.pci/cpp/wire_types.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/fit/defer.h>
#include <zircon/errors.h>

#include <bind/fuchsia/cpp/bind.h>

#include "src/devices/bus/drivers/pci/composite.h"
#include "src/devices/bus/drivers/pci/device.h"

#define RETURN_STATUS(level, status, format, ...)                                   \
  ({                                                                                \
    zx_status_t _status = (status);                                                 \
    zxlogf(level, "[%s] %s(" format ") = %s", device_->config()->addr(),            \
           __FUNCTION__ __VA_OPT__(, ) __VA_ARGS__, zx_status_get_string(_status)); \
    _status;                                                                        \
    return;                                                                         \
  })

#define RETURN_DEBUG(status, format...) RETURN_STATUS(DEBUG, status, format)
#define RETURN_TRACE(status, format...) RETURN_STATUS(TRACE, status, format)

namespace fpci = ::fuchsia_hardware_pci;

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace pci {

void FidlDevice::Bind(fidl::ServerEnd<fuchsia_hardware_pci::Device> request) {
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request), this);
}

zx::result<> FidlDevice::Create(zx_device_t* parent, pci::Device* device) {
  fbl::AllocChecker ac;
  std::unique_ptr<FidlDevice> fidl_dev(new (&ac) FidlDevice(parent, device));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }

  auto pci_bind_topo = static_cast<uint32_t>(
      BIND_PCI_TOPO_PACK(device->bus_id(), device->dev_id(), device->func_id()));

  zx_device_str_prop_t pci_device_props[] = {
      ddk::MakeStrProperty(bind_fuchsia::PCI_VID, static_cast<uint32_t>(device->vendor_id())),
      ddk::MakeStrProperty(bind_fuchsia::PCI_DID, static_cast<uint32_t>(device->device_id())),
      ddk::MakeStrProperty(bind_fuchsia::PCI_CLASS, static_cast<uint32_t>(device->class_id())),
      ddk::MakeStrProperty(bind_fuchsia::PCI_SUBCLASS, static_cast<uint32_t>(device->subclass())),
      ddk::MakeStrProperty(bind_fuchsia::PCI_INTERFACE, static_cast<uint32_t>(device->prog_if())),
      ddk::MakeStrProperty(bind_fuchsia::PCI_REVISION, static_cast<uint32_t>(device->rev_id())),
      ddk::MakeStrProperty(bind_fuchsia::PCI_TOPO, pci_bind_topo),
  };
  std::array offers = {
      fpci::Service::Name,
  };

  auto bus_info = std::make_unique<fdf::BusInfo>(fdf::BusInfo{{
      .bus = fdf::BusType::kPci,
      .address = fdf::DeviceAddress::WithArrayIntValue(
          {device->bus_id(), device->dev_id(), device->func_id()}),
      // TODO(surajmalhotra): Determine if device is soldered on or removable. For now we only
      // really run on devices with where everythign on the pci bus is pretty much permanent.
      .address_stability = fdf::DeviceAddressStability::kStable,
  }});

  zx::result result = fidl_dev->outgoing_dir().AddService<fuchsia_hardware_pci::Service>(
      fuchsia_hardware_pci::Service::InstanceHandler({
          .device = fidl_dev->bindings_.CreateHandler(
              fidl_dev.get(), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
              fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service the outgoing directory");
    return result.take_error();
  }

  result = fidl_dev->outgoing_dir().Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    return result.take_error();
  }

  // Create an isolated devhost to load the proxy pci driver containing the PciProxy
  // instance which will talk to this device.
  const auto name = std::string(device->config()->addr());
  zx_status_t status = fidl_dev->DdkAdd(ddk::DeviceAddArgs(name.c_str())
                                            .set_str_props(pci_device_props)
                                            .set_bus_info(std::move(bus_info))
                                            .set_flags(DEVICE_ADD_MUST_ISOLATE)
                                            .set_outgoing_dir(endpoints->client.TakeChannel())
                                            .set_fidl_service_offers(offers));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create pci fidl fragment %s: %s", device->config()->addr(),
           zx_status_get_string(status));
    return zx::error(status);
  }

  auto fidl_dev_unowned = fidl_dev.release();

  auto pci_info = CompositeInfo{
      .vendor_id = device->vendor_id(),
      .device_id = device->device_id(),
      .class_id = device->class_id(),
      .subclass = device->subclass(),
      .program_interface = device->prog_if(),
      .revision_id = device->rev_id(),
      .bus_id = device->bus_id(),
      .dev_id = device->dev_id(),
      .func_id = device->func_id(),
      .has_acpi = device->has_acpi(),
  };

  char spec_name[8];
  snprintf(spec_name, sizeof(spec_name), "%02x_%02x_%01x", device->bus_id(), device->dev_id(),
           device->func_id());
  status = fidl_dev_unowned->DdkAddCompositeNodeSpec(spec_name, CreateCompositeNodeSpec(pci_info));
  if (status != ZX_OK) {
    zxlogf(ERROR, "[%s] Failed to add pci composite spec: %s", device->config()->addr(),
           zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void FidlDevice::GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) {
  completer.Reply({.vendor_id = device_->vendor_id(),
                   .device_id = device_->device_id(),
                   .base_class = device_->class_id(),
                   .sub_class = device_->subclass(),
                   .program_interface = device_->prog_if(),
                   .revision_id = device_->rev_id(),
                   .bus_id = device_->bus_id(),
                   .dev_id = device_->dev_id(),
                   .func_id = device_->func_id()});
  RETURN_DEBUG(ZX_OK, "");
}

void FidlDevice::GetBar(GetBarRequestView request, GetBarCompleter::Sync& completer) {
  if (request->bar_id >= fpci::wire::kMaxBarCount) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    RETURN_DEBUG(ZX_ERR_INVALID_ARGS, "%u", request->bar_id);
  }

  fbl::AutoLock dev_lock(device_->dev_lock());
  auto& bar = device_->bars()[request->bar_id];
  if (!bar) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    RETURN_DEBUG(ZX_ERR_NOT_FOUND, "%u", request->bar_id);
  }

  size_t bar_size = bar->size;
  // If this device shares BAR data with either of the MSI-X tables then we need
  // to determine what portions of the BAR the driver can be permitted to
  // access. If the MSI-X bar exists in the only page present in the BAR then we
  // need to deny all access to the BAR.
  if (device_->capabilities().msix) {
    zx::result<size_t> result = device_->capabilities().msix->GetBarDataSize(*bar);
    if (result.is_error()) {
      completer.ReplyError(result.status_value());
      RETURN_DEBUG(result.status_value(), "%u", request->bar_id);
    }
    bar_size = result.value();
  }

  ZX_DEBUG_ASSERT(bar->allocation);
  switch (bar->allocation->type()) {
    case PCI_ADDRESS_SPACE_MEMORY: {
      zx::result<zx::vmo> result = bar->allocation->CreateVmo();
      if (result.is_ok()) {
        completer.ReplySuccess(
            {.bar_id = request->bar_id,
             .size = bar_size,
             .result = fpci::wire::BarResult::WithVmo(std::move(result.value()))});
        RETURN_DEBUG(ZX_OK, "%u", request->bar_id);
      }
    } break;
    case PCI_ADDRESS_SPACE_IO: {
      zx::result<zx::resource> result = bar->allocation->CreateResource();
      if (result.is_ok()) {
        fidl::Arena arena;
        completer.ReplySuccess(
            {.bar_id = request->bar_id,
             .size = bar_size,
             .result = fpci::wire::BarResult::WithIo(
                 arena, fuchsia_hardware_pci::wire::IoBar{.address = bar->address,
                                                          .resource = std::move(result.value())})});
        RETURN_DEBUG(ZX_OK, "%u", request->bar_id);
      }
    } break;
  }

  completer.ReplyError(ZX_ERR_BAD_STATE);
  RETURN_DEBUG(ZX_ERR_BAD_STATE, "%u", request->bar_id);
}

void FidlDevice::SetBusMastering(SetBusMasteringRequestView request,
                                 SetBusMasteringCompleter::Sync& completer) {
  fbl::AutoLock dev_lock(device_->dev_lock());
  zx_status_t status = device_->SetBusMastering(request->enabled);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    RETURN_DEBUG(status, "");
  }

  completer.ReplySuccess();
  RETURN_DEBUG(status, "");
}

void FidlDevice::ResetDevice(ResetDeviceCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  RETURN_DEBUG(ZX_ERR_NOT_SUPPORTED, "");
}

void FidlDevice::AckInterrupt(AckInterruptCompleter::Sync& completer) {
  fbl::AutoLock dev_lock(device_->dev_lock());
  zx_status_t status = device_->AckLegacyIrq();
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void FidlDevice::MapInterrupt(MapInterruptRequestView request,
                              MapInterruptCompleter::Sync& completer) {
  zx::result<zx::interrupt> result = device_->MapInterrupt(request->which_irq);
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
    RETURN_DEBUG(result.status_value(), "%#x", request->which_irq);
  }

  completer.ReplySuccess(std::move(result.value()));
  RETURN_DEBUG(result.status_value(), "%#x", request->which_irq);
}

void FidlDevice::SetInterruptMode(SetInterruptModeRequestView request,
                                  SetInterruptModeCompleter::Sync& completer) {
  zx_status_t status = device_->SetIrqMode(request->mode, request->requested_irq_count);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    RETURN_DEBUG(status, "%u, %#x", static_cast<uint8_t>(request->mode),
                 request->requested_irq_count);
  }

  completer.ReplySuccess();
  RETURN_DEBUG(status, "%u, %#x", static_cast<uint8_t>(request->mode),
               request->requested_irq_count);
}

void FidlDevice::GetInterruptModes(GetInterruptModesCompleter::Sync& completer) {
  pci_interrupt_modes_t modes = device_->GetInterruptModes();
  completer.Reply({.has_legacy = modes.has_legacy,
                   .msi_count = modes.msi_count,
                   .msix_count = modes.msix_count});
  RETURN_DEBUG(ZX_OK, "");
}

void FidlDevice::ReadConfig8(ReadConfig8RequestView request,
                             ReadConfig8Completer::Sync& completer) {
  auto result = device_->ReadConfig<uint8_t, PciReg8>(request->offset);
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
    RETURN_DEBUG(result.status_value(), "%#x", request->offset);
  }

  completer.ReplySuccess(result.value());
  RETURN_TRACE(result.status_value(), "%#x", request->offset);
}

void FidlDevice::ReadConfig16(ReadConfig16RequestView request,
                              ReadConfig16Completer::Sync& completer) {
  auto result = device_->ReadConfig<uint16_t, PciReg16>(request->offset);
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
    RETURN_DEBUG(result.status_value(), "%#x", request->offset);
  }

  completer.ReplySuccess(result.value());
  RETURN_TRACE(result.status_value(), "%#x", request->offset);
}

void FidlDevice::ReadConfig32(ReadConfig32RequestView request,
                              ReadConfig32Completer::Sync& completer) {
  auto result = device_->ReadConfig<uint32_t, PciReg32>(request->offset);
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
    RETURN_DEBUG(result.status_value(), "%#x", request->offset);
  }

  completer.ReplySuccess(result.value());
  RETURN_TRACE(result.status_value(), "%#x", request->offset);
}

void FidlDevice::WriteConfig8(WriteConfig8RequestView request,
                              WriteConfig8Completer::Sync& completer) {
  zx_status_t status = device_->WriteConfig<uint8_t, PciReg8>(request->offset, request->value);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    RETURN_DEBUG(status, "%#x, %#x", request->offset, request->value);
  }

  completer.ReplySuccess();
  RETURN_TRACE(status, "%#x, %#x", request->offset, request->value);
}

void FidlDevice::WriteConfig16(WriteConfig16RequestView request,
                               WriteConfig16Completer::Sync& completer) {
  zx_status_t status = device_->WriteConfig<uint16_t, PciReg16>(request->offset, request->value);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    RETURN_DEBUG(status, "%#x, %#x", request->offset, request->value);
  }

  completer.ReplySuccess();
  RETURN_TRACE(status, "%#x, %#x", request->offset, request->value);
}

void FidlDevice::WriteConfig32(WriteConfig32RequestView request,
                               WriteConfig32Completer::Sync& completer) {
  zx_status_t status = device_->WriteConfig<uint32_t, PciReg32>(request->offset, request->value);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    RETURN_DEBUG(status, "%#x, %#x", request->offset, request->value);
  }

  completer.ReplySuccess();
  RETURN_TRACE(status, "%#x, %#x", request->offset, request->value);
}

void FidlDevice::GetCapabilities(GetCapabilitiesRequestView request,
                                 GetCapabilitiesCompleter::Sync& completer) {
  std::vector<uint8_t> capabilities;
  {
    fbl::AutoLock dev_lock(device_->dev_lock());
    for (auto& capability : device_->capabilities().list) {
      if (capability.id() == static_cast<uint8_t>(request->id)) {
        capabilities.push_back(capability.base());
      }
    }
  }

  completer.Reply(::fidl::VectorView<uint8_t>::FromExternal(capabilities));
  RETURN_DEBUG(ZX_OK, "%#x", static_cast<uint8_t>(request->id));
}

void FidlDevice::GetExtendedCapabilities(GetExtendedCapabilitiesRequestView request,
                                         GetExtendedCapabilitiesCompleter::Sync& completer) {
  std::vector<uint16_t> ext_capabilities;
  {
    fbl::AutoLock dev_lock(device_->dev_lock());
    for (auto& ext_capability : device_->capabilities().ext_list) {
      if (ext_capability.id() == static_cast<uint16_t>(request->id)) {
        ext_capabilities.push_back(ext_capability.base());
      }
    }
  }

  completer.Reply(::fidl::VectorView<uint16_t>::FromExternal(ext_capabilities));
  RETURN_DEBUG(ZX_OK, "%#x", static_cast<uint16_t>(request->id));
}

void FidlDevice::GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) {
  fbl::AutoLock dev_lock(device_->dev_lock());
  zx::bti bti;
  zx_status_t status = device_->bdi()->GetBti(device_, request->index, &bti);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    RETURN_DEBUG(status, "%u", request->index);
  }

  completer.ReplySuccess(std::move(bti));
  RETURN_DEBUG(status, "%u", request->index);
}

}  // namespace pci
