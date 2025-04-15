// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/platform-bus.h"

#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <fidl/fuchsia.system.state/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/outgoing/cpp/handlers.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/object_view.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls/iommu.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <fbl/algorithm.h>

#include "src/devices/bus/drivers/platform/node-util.h"
#include "src/devices/bus/drivers/platform/platform_bus_config.h"

namespace platform_bus {

namespace {

namespace fhpb = fuchsia_hardware_platform_bus;
namespace fss = fuchsia_system_state;

zx::result<> ValidateResources(fhpb::Node& node) {
  if (node.name() == std::nullopt) {
    fdf::error("Node has no name?");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  std::replace(node.name()->begin(), node.name()->end(), ':', '_');

  if (node.mmio() != std::nullopt) {
    for (size_t i = 0; i < node.mmio()->size(); i++) {
      if (!IsValid(node.mmio().value()[i])) {
        fdf::error("node '{}' has invalid mmio {}", node.name().value(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.irq() != std::nullopt) {
    for (size_t i = 0; i < node.irq()->size(); i++) {
      if (!IsValid(node.irq().value()[i])) {
        fdf::error("node '{}' has invalid irq {}", node.name().value(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.bti() != std::nullopt) {
    for (size_t i = 0; i < node.bti()->size(); i++) {
      if (!IsValid(node.bti().value()[i])) {
        fdf::error("node '{}' has invalid bti {}", node.name().value(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.smc() != std::nullopt) {
    for (size_t i = 0; i < node.smc()->size(); i++) {
      if (!IsValid(node.smc().value()[i])) {
        fdf::error("node '{}' has invalid smc {}", node.name().value(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.metadata() != std::nullopt) {
    for (size_t i = 0; i < node.metadata()->size(); i++) {
      if (!IsValid(node.metadata().value()[i])) {
        fdf::error("node '{}' has invalid metadata {}", node.name().value(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.boot_metadata() != std::nullopt) {
    for (size_t i = 0; i < node.boot_metadata()->size(); i++) {
      if (!IsValid(node.boot_metadata().value()[i])) {
        fdf::error("node '{}' has invalid boot metadata {}", node.name().value(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  return zx::ok();
}

// Adds a passthrough device which forwards all banjo connections to the parent device.
// The device will be added as a child of |parent| with the name |name|, and |props| will
// be applied to the new device's add_args.
zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> AddProtocolPassthrough(
    const char* name, cpp20::span<const fuchsia_driver_framework::NodeProperty> props,
    platform_bus::PlatformBus* bus, fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent) {
  if (!bus || !name) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fhpb::Service::InstanceHandler handler({
      .platform_bus = bus->bindings().CreateHandler(bus, fdf::Dispatcher::GetCurrent()->get(),
                                                    fidl::kIgnoreBindingClosure),
      .iommu = bus->iommu_bindings().CreateHandler(bus, fdf::Dispatcher::GetCurrent()->get(),
                                                   fidl::kIgnoreBindingClosure),
      .firmware = bus->fw_bindings().CreateHandler(bus, fdf::Dispatcher::GetCurrent()->get(),
                                                   fidl::kIgnoreBindingClosure),
  });

  zx::result result = bus->outgoing()->AddService<fhpb::Service>(std::move(handler), name);
  if (result.is_error()) {
    return result.take_error();
  }

  result = bus->outgoing()->AddService<fuchsia_sysinfo::Service>(
      fuchsia_sysinfo::Service::InstanceHandler({
          .device = bus->sysinfo_bindings().CreateHandler(
              bus, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    return result.take_error();
  }

  std::array offers = {fdf::MakeOffer2<fhpb::Service>(name),
                       fdf::MakeOffer2<fuchsia_sysinfo::Service>(name),
                       fdf::MakeOffer2<fuchsia_driver_compat::Service>(name)};

  return fdf::AddChild(parent, bus->logger(), name, props, offers);
}

// Taken from src/lib/ddk/include/lib/ddk/device.h
#define DEVICE_SUSPEND_REASON_POWEROFF UINT8_C(0x10)
#define DEVICE_SUSPEND_REASON_SUSPEND_RAM UINT8_C(0x20)
#define DEVICE_SUSPEND_REASON_MEXEC UINT8_C(0x30)
#define DEVICE_SUSPEND_REASON_REBOOT UINT8_C(0x40)
#define DEVICE_SUSPEND_REASON_REBOOT_RECOVERY (UINT8_C(DEVICE_SUSPEND_REASON_REBOOT | 0x01))
#define DEVICE_SUSPEND_REASON_REBOOT_BOOTLOADER (UINT8_C(DEVICE_SUSPEND_REASON_REBOOT | 0x02))
#define DEVICE_SUSPEND_REASON_REBOOT_KERNEL_INITIATED (UINT8_C(DEVICE_SUSPEND_REASON_REBOOT | 0x03))
#define DEVICE_SUSPEND_REASON_SELECTIVE_SUSPEND UINT8_C(0x50)

uint8_t PowerStateToSuspendReason(fss::SystemPowerState power_state) {
  switch (power_state) {
    case fss::SystemPowerState::kReboot:
      return DEVICE_SUSPEND_REASON_REBOOT;
    case fss::SystemPowerState::kRebootRecovery:
      return DEVICE_SUSPEND_REASON_REBOOT_RECOVERY;
    case fss::SystemPowerState::kRebootBootloader:
      return DEVICE_SUSPEND_REASON_REBOOT_BOOTLOADER;
    case fss::SystemPowerState::kMexec:
      return DEVICE_SUSPEND_REASON_MEXEC;
    case fss::SystemPowerState::kPoweroff:
      return DEVICE_SUSPEND_REASON_POWEROFF;
    case fss::SystemPowerState::kSuspendRam:
      return DEVICE_SUSPEND_REASON_SUSPEND_RAM;
    case fss::SystemPowerState::kRebootKernelInitiated:
      return DEVICE_SUSPEND_REASON_REBOOT_KERNEL_INITIATED;
    default:
      return DEVICE_SUSPEND_REASON_SELECTIVE_SUSPEND;
  }
}

}  // anonymous namespace

zx::result<zx::bti> PlatformBus::GetBti(uint32_t iommu_index, uint32_t bti_id) {
  if (iommu_index != 0) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  std::pair key(iommu_index, bti_id);
  auto bti = cached_btis_.find(key);
  if (bti == cached_btis_.end()) {
    zx::bti new_bti;
    zx_status_t status = zx::bti::create(iommu_handle_, 0, bti_id, &new_bti);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    char name[ZX_MAX_NAME_LEN]{};
    std::format_to_n(name, sizeof(name) - 1, "pbus bti {:02x}:{:02x}", iommu_index, bti_id);
    status = new_bti.set_property(ZX_PROP_NAME, name, std::size(name));
    if (status != ZX_OK) {
      fdf::warn("Couldn't set name for BTI '{}': {}", std::string_view(name),
                zx_status_get_string(status));
    }
    auto [iter, _] = cached_btis_.emplace(key, std::move(new_bti));
    bti = iter;
  }

  zx::bti out_bti;
  zx_status_t status = bti->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_bti);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(out_bti));
}

zx::unowned_resource PlatformBus::GetIrqResource() const {
  static zx::resource irq_resource;
  if (!irq_resource.is_valid()) {
    zx::result client = incoming()->Connect<fuchsia_kernel::IrqResource>();
    if (client.is_ok()) {
      fidl::Result resource = fidl::Call(*client)->Get();
      if (resource.is_ok()) {
        irq_resource = std::move(resource.value().resource());
      }
    }
  }
  return irq_resource.borrow();
}

zx::unowned_resource PlatformBus::GetMmioResource() const {
  static zx::resource mmio_resource;
  if (!mmio_resource.is_valid()) {
    zx::result client = incoming()->Connect<fuchsia_kernel::MmioResource>();
    if (client.is_ok()) {
      fidl::Result resource = fidl::Call(*client)->Get();
      if (resource.is_ok()) {
        mmio_resource = std::move(resource.value().resource());
      }
    }
  }
  return mmio_resource.borrow();
}

zx::unowned_resource PlatformBus::GetSmcResource() const {
  static zx::resource smc_resource;
  if (!smc_resource.is_valid()) {
    zx::result client = incoming()->Connect<fuchsia_kernel::SmcResource>();
    if (client.is_ok()) {
      fidl::Result resource = fidl::Call(*client)->Get();
      if (resource.is_ok()) {
        smc_resource = std::move(resource.value().resource());
      }
    }
  }
  return smc_resource.borrow();
}

void PlatformBus::NodeAdd(NodeAddRequestView request, fdf::Arena& arena,
                          NodeAddCompleter::Sync& completer) {
  if (!request->node.has_name()) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  auto natural = fidl::ToNatural(request->node);
  completer.buffer(arena).Reply(NodeAddInternal(natural));
}

zx::result<> PlatformBus::NodeAddInternal(fhpb::Node& node) {
  auto result = ValidateResources(node);
  if (result.is_error()) {
    fdf::error("Failed to validate resources: {}", result);
    return result.take_error();
  }

  zx::result dev = PlatformDevice::Create(std::move(node), this, inspector());
  if (dev.is_error()) {
    fdf::error("Failed to create platform device: {}", dev);
    return dev.take_error();
  }

  if (zx::result result = dev->CreateNode(); result.is_error()) {
    fdf::error("Failed to create platform device node: {}", result);
    return result.take_error();
  }

  devices_.push_back(std::move(dev.value()));
  return zx::ok();
}

void PlatformBus::GetBoardName(GetBoardNameCompleter::Sync& completer) {
  // Reply immediately if board_name is valid.
  if (!board_info_.board_name().empty()) {
    completer.Reply(ZX_OK, fidl::StringView::FromExternal(board_info_.board_name()));
    return;
  }
  // Cache the requests until board_name becomes valid.
  board_name_completer_.push_back(completer.ToAsync());
}

void PlatformBus::GetBoardRevision(GetBoardRevisionCompleter::Sync& completer) {
  completer.Reply(ZX_OK, board_info_.board_revision());
}

void PlatformBus::GetBootloaderVendor(GetBootloaderVendorCompleter::Sync& completer) {
  // Reply immediately if vendor is valid.
  if (bootloader_info_.vendor() != std::nullopt) {
    completer.Reply(ZX_OK, fidl::StringView::FromExternal(bootloader_info_.vendor().value()));
    return;
  }
  // Cache the requests until vendor becomes valid.
  bootloader_vendor_completer_.push_back(completer.ToAsync());
}

void PlatformBus::GetInterruptControllerInfo(GetInterruptControllerInfoCompleter::Sync& completer) {
  fuchsia_sysinfo::wire::InterruptControllerInfo info = {
      .type = interrupt_controller_type_,
  };
  completer.Reply(
      ZX_OK, fidl::ObjectView<fuchsia_sysinfo::wire::InterruptControllerInfo>::FromExternal(&info));
}

void PlatformBus::GetSerialNumber(GetSerialNumberCompleter::Sync& completer) {
  auto result = GetBootItem(ZBI_TYPE_SERIAL_NUMBER, {});
  if (result.is_error()) {
    fdf::info("Boot Item ZBI_TYPE_SERIAL_NUMBER not found");
    completer.ReplyError(result.error_value());
    return;
  }
  auto& [vmo, length] = result.value()[0];
  if (length > fuchsia_sysinfo::wire::kSerialNumberLen) {
    completer.ReplyError(ZX_ERR_BUFFER_TOO_SMALL);
    return;
  }
  char serial[fuchsia_sysinfo::wire::kSerialNumberLen];
  zx_status_t status = vmo.read(serial, 0, length);
  if (status != ZX_OK) {
    fdf::error("Failed to read serial number VMO {}", status);
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(fidl::StringView::FromExternal(serial, length));
}

void PlatformBus::GetBoardInfo(fdf::Arena& arena, GetBoardInfoCompleter::Sync& completer) {
  fidl::Arena<> fidl_arena;
  completer.buffer(arena).ReplySuccess(fidl::ToWire(fidl_arena, board_info_));
}

void PlatformBus::SetBoardInfo(SetBoardInfoRequestView request, fdf::Arena& arena,
                               SetBoardInfoCompleter::Sync& completer) {
  auto& info = request->info;
  if (info.has_board_name()) {
    board_info_.board_name() = info.board_name().get();
    fdf::info("PlatformBus: set board name to \"{}\"", board_info_.board_name());

    std::vector<GetBoardNameCompleter::Async> completer_tmp_;
    // Respond to pending boardname requests, if any.
    board_name_completer_.swap(completer_tmp_);
    while (!completer_tmp_.empty()) {
      completer_tmp_.back().Reply(ZX_OK, fidl::StringView::FromExternal(board_info_.board_name()));
      completer_tmp_.pop_back();
    }
  }
  if (info.has_board_revision()) {
    board_info_.board_revision() = info.board_revision();
  }
  completer.buffer(arena).ReplySuccess();
}

void PlatformBus::SetBootloaderInfo(SetBootloaderInfoRequestView request, fdf::Arena& arena,
                                    SetBootloaderInfoCompleter::Sync& completer) {
  auto& info = request->info;
  if (info.has_vendor()) {
    bootloader_info_.vendor() = info.vendor().get();
    fdf::info("PlatformBus: set bootloader vendor to \"{}\"", bootloader_info_.vendor().value());

    std::vector<GetBootloaderVendorCompleter::Async> completer_tmp_;
    // Respond to pending boardname requests, if any.
    bootloader_vendor_completer_.swap(completer_tmp_);
    while (!completer_tmp_.empty()) {
      completer_tmp_.back().Reply(
          ZX_OK, fidl::StringView::FromExternal(bootloader_info_.vendor().value()));
      completer_tmp_.pop_back();
    }
  }
  completer.buffer(arena).ReplySuccess();
}

void PlatformBus::RegisterSysSuspendCallback(RegisterSysSuspendCallbackRequestView request,
                                             fdf::Arena& arena,
                                             RegisterSysSuspendCallbackCompleter::Sync& completer) {
  suspend_cb_.Bind(std::move(request->suspend_cb),
                   fdf::Dispatcher::GetCurrent()->async_dispatcher());
  completer.buffer(arena).ReplySuccess();
}

void PlatformBus::AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request, fdf::Arena& arena,
                                       AddCompositeNodeSpecCompleter::Sync& completer) {
  // Create the pdev fragments
  auto vid = request->node.has_vid() ? request->node.vid() : 0;
  auto pid = request->node.has_pid() ? request->node.pid() : 0;
  auto did = request->node.has_did() ? request->node.did() : 0;
  auto instance_id = request->node.has_instance_id() ? request->node.instance_id() : 0;

  fuchsia_driver_framework::CompositeNodeSpec composite_node_spec = fidl::ToNatural(request->spec);
  if (!composite_node_spec.parents2().has_value()) {
    composite_node_spec.parents2().emplace();
  }
  composite_node_spec.parents2()->push_back(fuchsia_driver_framework::ParentSpec2{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule2(bind_fuchsia::PROTOCOL,
                                       bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
              fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_VID, vid),
              fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_PID, pid),
              fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_DID, did),
              fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id),
          },
      .properties =
          {
              fdf::MakeProperty2(bind_fuchsia::PROTOCOL,
                                 bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
              fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_VID, vid),
              fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_PID, pid),
              fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_DID, did),
              fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id),
          },
  }});
  zx::result composite_node_manager =
      incoming()->Connect<fuchsia_driver_framework::CompositeNodeManager>();
  if (composite_node_manager.is_error()) {
    fdf::error("Failed to connect to CompositeNodeManager: {}", composite_node_manager);
    completer.buffer(arena).ReplyError(composite_node_manager.status_value());
    return;
  }

  fidl::Result result = fidl::Call(*composite_node_manager)->AddSpec(composite_node_spec);
  if (result.is_error()) {
    fdf::error("Failed to add composite node spec {}", result.error_value().FormatDescription());
    if (result.error_value().is_framework_error()) {
      completer.buffer(arena).ReplyError(result.error_value().framework_error().status());
    } else {
      completer.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
    }
    return;
  }

  // Create a platform device for the node.
  auto natural = fidl::ToNatural(request->node);
  auto valid = ValidateResources(natural);
  if (valid.is_error()) {
    fdf::error("Failed to validate resources: {}", valid);
    completer.buffer(arena).ReplyError(valid.error_value());
    return;
  }

  zx::result dev = PlatformDevice::Create(std::move(natural), this, inspector());
  if (dev.is_error()) {
    fdf::error("Failed to create platform device: {}", dev);
    completer.buffer(arena).ReplyError(dev.status_value());
    return;
  }
  if (zx::result result = dev->CreateNode(); result.is_error()) {
    fdf::error("Failed to create platform device node: {}", result);
    completer.buffer(arena).ReplyError(result.status_value());
    return;
  }
  devices_.push_back(std::move(dev.value()));

  completer.buffer(arena).ReplySuccess();
}

void PlatformBus::GetBti(GetBtiRequestView request, fdf::Arena& arena,
                         GetBtiCompleter::Sync& completer) {
  zx::result bti = GetBti(request->iommu_index, request->bti_id);

  if (bti.is_error()) {
    completer.buffer(arena).ReplyError(bti.status_value());
    return;
  }

  completer.buffer(arena).ReplySuccess(std::move(bti.value()));
}

void PlatformBus::GetFirmware(GetFirmwareRequestView request, fdf::Arena& arena,
                              GetFirmwareCompleter::Sync& completer) {
  uint32_t type = 0;
  switch (request->type) {
    case fhpb::wire::FirmwareType::kDeviceTree:
      type = ZBI_TYPE_DEVICETREE;
      break;
    case fhpb::wire::FirmwareType::kAcpi:
      type = ZBI_TYPE_ACPI_RSDP;
      break;
    case fhpb::wire::FirmwareType::kSmbios:
      type = ZBI_TYPE_SMBIOS;
      break;
    default:
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
      return;
  }
  zx::result result = GetBootItem(type, {});
  if (result.is_error()) {
    fdf::warn("Platform GetBootItem failed {}", result);
    completer.buffer(arena).ReplyError(result.status_value());
    return;
  }
  fidl::VectorView<fhpb::wire::FirmwareBlob> ret(arena, result->size());
  for (size_t i = 0; i < result->size(); i++) {
    auto& [vmo, length] = result.value()[i];
    ret[i] = fhpb::wire::FirmwareBlob{
        .vmo = std::move(vmo),
        .length = length,
    };
  }
  completer.buffer(arena).ReplySuccess(ret);
}

void PlatformBus::handle_unknown_method(fidl::UnknownMethodMetadata<fhpb::PlatformBus> metadata,
                                        fidl::UnknownMethodCompleter::Sync& completer) {
  fdf::warn("PlatformBus received unknown method with ordinal: {}", metadata.method_ordinal);
}

zx::result<std::vector<PlatformBus::BootItemResult>> PlatformBus::GetBootItem(
    uint32_t type, std::optional<uint32_t> extra) {
  fidl::Arena arena;
  fidl::ObjectView<fuchsia_boot::wire::Extra> extra_struct;
  if (extra.has_value()) {
    extra_struct = fidl::ObjectView<fuchsia_boot::wire::Extra>(arena, extra.value());
  };
  auto result = fidl::WireCall(items_svc_)->Get2(type, extra_struct);
  if (!result.ok()) {
    return zx::error(result.status());
  }
  if (result->is_error()) {
    if (result->error_value() == ZX_ERR_NOT_SUPPORTED) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    return zx::error(result->error_value());
  }
  fidl::VectorView items = result->value()->retrieved_items;
  if (items.count() == 0) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  std::vector<PlatformBus::BootItemResult> ret;
  ret.reserve(items.count());
  for (size_t i = 0; i < items.count(); i++) {
    ret.emplace_back(PlatformBus::BootItemResult{
        .vmo = std::move(items[i].payload),
        .length = items[i].length,
    });
  }
  return zx::ok(std::move(ret));
}

zx::result<fbl::Array<uint8_t>> PlatformBus::GetBootItemArray(uint32_t type,
                                                              std::optional<uint32_t> extra) {
  zx::result result = GetBootItem(type, extra);
  if (result.is_error()) {
    return result.take_error();
  }
  if (result->size() > 1) {
    fdf::warn("Found multiple boot items of type: {}", type);
  }
  auto& [vmo, length] = result.value()[0];
  fbl::Array<uint8_t> data(new uint8_t[length], length);
  zx_status_t status = vmo.read(data.data(), 0, data.size());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(data));
}

PlatformBus::PlatformBus(fdf::DriverStartArgs start_args,
                         fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("platform-bus", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> PlatformBus::Start() {
  zx::result sys = AddOwnedChild("sys");
  if (sys.is_error()) {
    fdf::error("Failed to create sys with error {}", sys);
    return sys.take_error();
  }
  sys_node_ = std::move(sys.value());

  zx::result items_svc = incoming()->Connect<fuchsia_boot::Items>();
  if (items_svc.is_error()) {
    fdf::error("Failed to get connect to boot items {}", items_svc);
    return items_svc.take_error();
  }
  items_svc_ = std::move(items_svc.value());

  // Set up a dummy IOMMU protocol to use in the case where our board driver
  // does not set a real one.
  zx_iommu_desc_dummy_t desc;

  zx::result iommu_client = incoming()->Connect<fuchsia_kernel::IommuResource>();
  if (iommu_client.is_ok()) {
    auto result = fidl::Call(iommu_client.value())->Get();
    if (result.is_ok()) {
      zx_status_t status = zx::iommu::create(result->resource(), ZX_IOMMU_TYPE_DUMMY, &desc,
                                             sizeof(desc), &iommu_handle_);
      if (status != ZX_OK) {
        fdf::error("Failed to get get create iommu {}", zx_status_get_string(status));
        return zx::error(status);
      }
    }
  }

  // Read kernel driver.
#if __x86_64__
  interrupt_controller_type_ = fuchsia_sysinfo::wire::InterruptControllerType::kApic;
#else
  std::array<std::pair<zbi_kernel_driver_t, fuchsia_sysinfo::wire::InterruptControllerType>, 3>
      interrupt_driver_type_mapping = {
          {{ZBI_KERNEL_DRIVER_ARM_GIC_V2, fuchsia_sysinfo::wire::InterruptControllerType::kGicV2},
           {ZBI_KERNEL_DRIVER_ARM_GIC_V3, fuchsia_sysinfo::wire::InterruptControllerType::kGicV3},
           {ZBI_KERNEL_DRIVER_RISCV_PLIC, fuchsia_sysinfo::wire::InterruptControllerType::kPlic}},
      };

  for (const auto& [driver, controller] : interrupt_driver_type_mapping) {
    auto boot_item = GetBootItem(ZBI_TYPE_KERNEL_DRIVER, driver);
    if (boot_item.is_error() && boot_item.status_value() != ZX_ERR_NOT_FOUND) {
      return boot_item.take_error();
    }
    if (boot_item.is_ok()) {
      interrupt_controller_type_ = controller;
    }
  }
#endif

  // Read platform ID.
  zx::result platform_id_result = GetBootItem(ZBI_TYPE_PLATFORM_ID, {});
  if (platform_id_result.is_error() && platform_id_result.status_value() != ZX_ERR_NOT_FOUND) {
    return platform_id_result.take_error();
  }

#if __aarch64__
  {
    // For arm64, we do not expect a board to set the bootloader info.
    bootloader_info_.vendor() = "<unknown>";
  }
#endif

  if (platform_id_result.is_ok()) {
    if (platform_id_result.value()[0].length != sizeof(zbi_platform_id_t)) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    zbi_platform_id_t platform_id;
    zx_status_t status =
        platform_id_result.value()[0].vmo.read(&platform_id, 0, sizeof(platform_id));
    if (status != ZX_OK) {
      return zx::error(status);
    }
    fdf::info("VID: {} PID: {} board: \"{}\"", platform_id.vid, platform_id.pid,
              std::string_view(platform_id.board_name));
    board_info_.vid() = platform_id.vid;
    board_info_.pid() = platform_id.pid;
    board_info_.board_name() = platform_id.board_name;
  } else {
#if __x86_64__
    // For x64, we might not find the ZBI_TYPE_PLATFORM_ID, old bootloaders
    // won't support this, for example. If this is the case, cons up the VID/PID
    // here to allow the acpi board driver to load and bind.
    board_info_.vid() = PDEV_VID_INTEL;
    board_info_.pid() = PDEV_PID_X86;
#else
    fdf::error("ZBI_TYPE_PLATFORM_ID not found");
    return zx::error(ZX_ERR_INTERNAL);
#endif
  }

  // Set default board_revision.
  zx::result zbi_board_info = GetBoardInfo();
  if (zbi_board_info.is_ok()) {
    board_info_.board_revision() = zbi_board_info->revision;
  }

  // Then we attach the platform-bus device below it.
  zx::result platform_node = fdf::AddOwnedChild(sys_node_.node_, logger(), "platform");
  if (platform_node.is_error()) {
    return platform_node.take_error();
  }
  platform_node_ = std::move(platform_node.value());

  std::array passthrough_props = {
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID, board_info_.vid()),
      fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID, board_info_.pid()),
  };

  device_server_.Init("pt");
  device_server_.Serve(dispatcher(), outgoing().get());

  zx::result board_data = GetBootItemArray(ZBI_TYPE_DRV_BOARD_PRIVATE, {});
  if (board_data.is_error() && board_data.status_value() != ZX_ERR_NOT_FOUND) {
    return board_data.take_error();
  }
  if (board_data.is_ok()) {
    zx_status_t status = device_server_.AddMetadata(DEVICE_METADATA_BOARD_PRIVATE,
                                                    board_data->data(), board_data->size());
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  zx::result pt_node =
      AddProtocolPassthrough("pt", passthrough_props, this, platform_node_.node_.borrow());
  if (pt_node.is_error()) {
    // We log the error but we do nothing as we've already added the device successfully.
    fdf::error("Error while adding pt: {}", pt_node);
  } else {
    pt_node_ = std::move(pt_node.value());
  }

  auto config = take_config<platform_bus_config::Config>();
  if (config.software_device_ids().size() != config.software_device_names().size()) {
    fdf::error(
        "Invalid config. software_device_ids and software_device_names must have same length");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  for (size_t i = 0; i < config.software_device_ids().size(); i++) {
    fhpb::Node device = {};
    device.name() = config.software_device_names()[i];
    device.vid() = PDEV_VID_GENERIC;
    device.pid() = PDEV_PID_GENERIC;
    device.did() = config.software_device_ids()[i];
    if (zx::result result = NodeAddInternal(device); result.is_error()) {
      return result.take_error();
    }
  }

  suspend_enabled_ = config.suspend_enabled();

  return zx::ok();
}

void PlatformBus::PrepareStop(fdf::PrepareStopCompleter completer) {
  if (suspend_cb().is_valid()) {
    auto client = incoming()->Connect<fss::SystemStateTransition>();
    if (client.is_error()) {
      fdf::error("Failed to connect to fuchsia.system.state/SystemStateTransition: {}", client);
      completer(client.take_error());
      return;
    }
    fidl::Result state = fidl::Call(*client)->GetTerminationSystemState();
    if (state.is_error()) {
      fdf::error("Failed to connect to get termination state: {}", state.error_value());
      completer(zx::error(state.error_value().status()));
      return;
    }
    suspend_cb()
        ->Callback(false, PowerStateToSuspendReason(state->state()))
        .ThenExactlyOnce([completer = std::move(completer)](
                             fidl::WireUnownedResult<fhpb::SysSuspend::Callback>& status) mutable {
          if (!status.ok()) {
            completer(zx::error(status.status()));
            return;
          }
          if (status->out_status != ZX_OK) {
            completer(zx::error(status->out_status));
            return;
          }
          completer(zx::ok());
        });
    return;
  }
  completer(zx::ok());
}

zx::result<zbi_board_info_t> PlatformBus::GetBoardInfo() {
  zx::result result = GetBootItem(ZBI_TYPE_DRV_BOARD_INFO, {});
  if (result.is_error()) {
    // This is expected on some boards.
    fdf::info("Boot Item ZBI_TYPE_DRV_BOARD_INFO not found");
    return result.take_error();
  }
  auto& [vmo, length] = result.value()[0];
  if (length != sizeof(zbi_board_info_t)) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  zbi_board_info_t board_info;
  zx_status_t status = vmo.read(&board_info, 0, length);
  if (status != ZX_OK) {
    fdf::error("Failed to read zbi_board_info_t VMO: {}", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(board_info);
}

}  // namespace platform_bus

FUCHSIA_DRIVER_EXPORT(platform_bus::PlatformBus);
