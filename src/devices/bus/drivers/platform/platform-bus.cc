// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/platform-bus.h"

#include <assert.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/outgoing/cpp/handlers.h>
#include <lib/fdf/dispatcher.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls/iommu.h>

#include <algorithm>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <ddktl/fidl.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "src/devices/bus/drivers/platform/node-util.h"

namespace {
// Adds a passthrough device which forwards all banjo connections to the parent device.
// The device will be added as a child of |parent| with the name |name|, and |props| will
// be applied to the new device's add_args.
// Returns ZX_OK if the device is successfully added.
zx_status_t AddProtocolPassthrough(const char* name, cpp20::span<const zx_device_prop_t> props,
                                   platform_bus::PlatformBus* parent, zx_device_t** out_device) {
  if (!parent || !name) {
    return ZX_ERR_INVALID_ARGS;
  }

  static zx_protocol_device_t passthrough_proto = {
      .version = DEVICE_OPS_VERSION,
      .get_protocol =
          [](void* ctx, uint32_t id, void* proto) {
            return device_get_protocol(reinterpret_cast<platform_bus::PlatformBus*>(ctx)->zxdev(),
                                       id, proto);
          },
      .release = [](void* ctx) {},
  };

  fuchsia_hardware_platform_bus::Service::InstanceHandler handler({
      .platform_bus = parent->bindings().CreateHandler(parent, fdf::Dispatcher::GetCurrent()->get(),
                                                       fidl::kIgnoreBindingClosure),
  });

  auto status =
      parent->outgoing().AddService<fuchsia_hardware_platform_bus::Service>(std::move(handler));
  if (status.is_error()) {
    return status.error_value();
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  auto result = parent->outgoing().Serve(std::move(endpoints->server));
  if (result.is_error()) {
    return result.error_value();
  }

  std::array offers = {
      fuchsia_hardware_platform_bus::Service::Name,
  };

  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = name,
      .ctx = parent,
      .ops = &passthrough_proto,
      .props = props.data(),
      .prop_count = static_cast<uint32_t>(props.size()),
      .runtime_service_offers = offers.data(),
      .runtime_service_offer_count = offers.size(),
      .outgoing_dir_channel = endpoints->client.TakeChannel().release(),
  };

  return device_add(parent->zxdev(), &args, out_device);
}

device_bind_prop_key_t ConvertFidlPropertyKey(
    const fuchsia_driver_framework::NodePropertyKey& key) {
  switch (key.Which()) {
    case fuchsia_driver_framework::NodePropertyKey::Tag::kIntValue:
      return device_bind_prop_int_key(key.int_value().value());
    case fuchsia_driver_framework::NodePropertyKey::Tag::kStringValue:
      return device_bind_prop_str_key(key.string_value().value().c_str());
  }
}

zx::result<device_bind_prop_value_t> ConvertFidlPropertyValue(
    const fuchsia_driver_framework::NodePropertyValue& value) {
  switch (value.Which()) {
    case fuchsia_driver_framework::NodePropertyValue::Tag::kIntValue:
      return zx::ok(device_bind_prop_int_val(value.int_value().value()));
    case fuchsia_driver_framework::NodePropertyValue::Tag::kStringValue:
      return zx::ok(device_bind_prop_str_val(value.string_value().value().c_str()));
    case fuchsia_driver_framework::NodePropertyValue::Tag::kBoolValue:
      return zx::ok(device_bind_prop_bool_val(value.bool_value().value()));
    case fuchsia_driver_framework::NodePropertyValue::Tag::kEnumValue:
      return zx::ok(device_bind_prop_enum_val(value.enum_value().value().c_str()));
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  }
}

zx::result<ddk::BindRule> ConvertFidlBindRule(const fuchsia_driver_framework::BindRule& fidl_rule) {
  auto key = ConvertFidlPropertyKey(fidl_rule.key());

  std::vector<device_bind_prop_value_t> values;
  values.reserve(fidl_rule.values().size());
  for (const auto& fidl_value : fidl_rule.values()) {
    auto property_value = ConvertFidlPropertyValue(fidl_value);
    if (property_value.is_error()) {
      return property_value.take_error();
    }
    values.push_back(property_value.value());
  }

  device_bind_rule_condition condition;
  switch (fidl_rule.condition()) {
    case fuchsia_driver_framework::Condition::kAccept:
      condition = DEVICE_BIND_RULE_CONDITION_ACCEPT;
      break;
    case fuchsia_driver_framework::Condition::kReject:
      condition = DEVICE_BIND_RULE_CONDITION_REJECT;
      break;
    case fuchsia_driver_framework::Condition::kUnknown:
      return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(ddk::BindRule(key, condition, values));
}

zx::result<> AppendParentSpecs(ddk::CompositeNodeSpec& spec,
                               const std::vector<fuchsia_driver_framework::ParentSpec>& parents) {
  for (const auto& parent : parents) {
    if (parent.bind_rules().empty()) {
      zxlogf(ERROR, "Parent spec bind rules cannot be empty");
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    if (parent.properties().empty()) {
      zxlogf(ERROR, "Parent spec properties cannot be empty");
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    std::vector<ddk::BindRule> rules;
    rules.reserve(parent.bind_rules().size());
    for (const auto& bind_rule : parent.bind_rules()) {
      auto result = ConvertFidlBindRule(bind_rule);
      if (result.is_error()) {
        return zx::error(result.status_value());
      }
      rules.push_back(result.value());
    }

    std::vector<device_bind_prop_t> properties;
    properties.reserve(parent.properties().size());
    for (const auto& property : parent.properties()) {
      auto property_value = ConvertFidlPropertyValue(property.value());
      if (property_value.is_error()) {
        zxlogf(ERROR, "Invalid property value");
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      properties.push_back(device_bind_prop_t{
          .key = ConvertFidlPropertyKey(property.key()),
          .value = property_value.value(),
      });
    }
    spec.AddParentSpec(rules, properties);
  }
  return zx::ok();
}

}  // anonymous namespace

namespace platform_bus {

zx_status_t PlatformBus::IommuGetBti(uint32_t iommu_index, uint32_t bti_id, zx::bti* out_bti) {
  if (iommu_index != 0) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::pair key(iommu_index, bti_id);
  auto bti = cached_btis_.find(key);
  if (bti == cached_btis_.end()) {
    zx::bti new_bti;
    zx_status_t status = zx::bti::create(iommu_handle_, 0, bti_id, &new_bti);
    if (status != ZX_OK) {
      return status;
    }

    char name[ZX_MAX_NAME_LEN]{};
    snprintf(name, std::size(name) - 1, "pbus bti %02x:%02x", iommu_index, bti_id);
    status = new_bti.set_property(ZX_PROP_NAME, name, std::size(name));
    if (status != ZX_OK) {
      zxlogf(WARNING, "Couldn't set name for BTI '%s': %s", name, zx_status_get_string(status));
    }
    auto [iter, _] = cached_btis_.emplace(key, std::move(new_bti));
    bti = iter;
  }

  return bti->second.duplicate(ZX_RIGHT_SAME_RIGHTS, out_bti);
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

zx::result<> PlatformBus::NodeAddInternal(fuchsia_hardware_platform_bus::Node& node) {
  auto result = ValidateResources(node);
  if (result.is_error()) {
    return result.take_error();
  }
  std::unique_ptr<platform_bus::PlatformDevice> dev;
  auto status =
      PlatformDevice::Create(std::move(node), zxdev(), this, PlatformDevice::Isolated, &dev);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  status = dev->Start();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = dev.release();
  return zx::ok();
}

void PlatformBus::GetBoardName(GetBoardNameCompleter::Sync& completer) {
  fbl::AutoLock lock(&board_info_lock_);
  // Reply immediately if board_name is valid.
  if (!board_info_.board_name().empty()) {
    completer.Reply(ZX_OK, fidl::StringView::FromExternal(board_info_.board_name()));
    return;
  }
  // Cache the requests until board_name becomes valid.
  board_name_completer_.push_back(completer.ToAsync());
}

void PlatformBus::GetBoardRevision(GetBoardRevisionCompleter::Sync& completer) {
  fbl::AutoLock lock(&board_info_lock_);
  completer.Reply(ZX_OK, board_info_.board_revision());
}

void PlatformBus::GetBootloaderVendor(GetBootloaderVendorCompleter::Sync& completer) {
  fbl::AutoLock lock(&bootloader_info_lock_);
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
  auto result = GetBootItem(ZBI_TYPE_SERIAL_NUMBER, 0);
  if (result.is_error()) {
    zxlogf(INFO, "Boot Item ZBI_TYPE_SERIAL_NUMBER not found");
    completer.ReplyError(result.error_value());
    return;
  }
  auto& [vmo, length] = *result;
  if (length > fuchsia_sysinfo::wire::kSerialNumberLen) {
    completer.ReplyError(ZX_ERR_BUFFER_TOO_SMALL);
    return;
  }
  char serial[fuchsia_sysinfo::wire::kSerialNumberLen];
  zx_status_t status = vmo.read(serial, 0, length);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to read serial number VMO %d", status);
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(fidl::StringView::FromExternal(serial, length));
}

void PlatformBus::GetBoardInfo(fdf::Arena& arena, GetBoardInfoCompleter::Sync& completer) {
  fbl::AutoLock lock(&board_info_lock_);
  fidl::Arena<> fidl_arena;
  completer.buffer(arena).ReplySuccess(fidl::ToWire(fidl_arena, board_info_));
}

void PlatformBus::SetBoardInfo(SetBoardInfoRequestView request, fdf::Arena& arena,
                               SetBoardInfoCompleter::Sync& completer) {
  fbl::AutoLock lock(&board_info_lock_);
  auto& info = request->info;
  if (info.has_board_name()) {
    board_info_.board_name() = info.board_name().get();
    zxlogf(INFO, "PlatformBus: set board name to \"%s\"", board_info_.board_name().data());

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
  fbl::AutoLock lock(&bootloader_info_lock_);
  auto& info = request->info;
  if (info.has_vendor()) {
    bootloader_info_.vendor() = info.vendor().get();
    zxlogf(INFO, "PlatformBus: set bootloader vendor to \"%s\"", bootloader_info_.vendor()->data());

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

  const ddk::BindRule kPDevBindRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID, vid),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID, pid),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID, did),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id),
  };

  const device_bind_prop_t kPDevProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID, vid),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID, pid),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID, did),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id),
  };

  auto composite_node_spec = ddk::CompositeNodeSpec(kPDevBindRules, kPDevProperties);

  auto fidl_spec = fidl::ToNatural(request->spec);
  if (fidl_spec.parents().has_value()) {
    auto result = AppendParentSpecs(composite_node_spec, fidl_spec.parents().value());
    if (result.is_error()) {
      completer.buffer(arena).ReplyError(result.status_value());
      return;
    }
  }

  auto status = DdkAddCompositeNodeSpec(fidl_spec.name()->c_str(), composite_node_spec);

  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec failed %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  // Create a platform device for the node.
  std::unique_ptr<platform_bus::PlatformDevice> dev;
  auto natural = fidl::ToNatural(request->node);
  auto valid = ValidateResources(natural);
  if (valid.is_error()) {
    completer.buffer(arena).ReplyError(valid.error_value());
    return;
  }

  status =
      PlatformDevice::Create(std::move(natural), zxdev(), this, PlatformDevice::Fragment, &dev);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  status = dev->Start();
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dev_ptr = dev.release();

  completer.buffer(arena).ReplySuccess();
}

zx::result<PlatformBus::BootItemResult> PlatformBus::GetBootItem(uint32_t type, uint32_t extra) {
  auto result = fidl::WireCall(items_svc_)->Get(type, extra);
  if (!result.ok()) {
    return zx::error(result.status());
  }
  if (!result->payload.is_valid()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return zx::ok(PlatformBus::BootItemResult{
      .vmo = std::move(result->payload),
      .length = result->length,
  });
}

zx::result<fbl::Array<uint8_t>> PlatformBus::GetBootItemArray(uint32_t type, uint32_t extra) {
  zx::result result = GetBootItem(type, extra);
  if (result.is_error()) {
    return result.take_error();
  }
  auto& [vmo, length] = *result;
  fbl::Array<uint8_t> data(new uint8_t[length], length);
  zx_status_t status = vmo.read(data.data(), 0, data.size());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(data));
}

void PlatformBus::DdkRelease() { delete this; }

typedef struct {
  void* pbus_instance;
  zx_device_t* sys_root;
} sysdev_suspend_t;

static void sys_device_suspend(void* ctx, uint8_t requested_state, bool enable_wake,
                               uint8_t suspend_reason) {
  auto* p = reinterpret_cast<sysdev_suspend_t*>(ctx);
  auto* pbus = reinterpret_cast<class PlatformBus*>(p->pbus_instance);

  if (pbus != nullptr) {
    auto& suspend_cb = pbus->suspend_cb();
    if (suspend_cb.is_valid()) {
      suspend_cb->Callback(requested_state, enable_wake, suspend_reason)
          .ThenExactlyOnce(
              [sys_root = p->sys_root](
                  fidl::WireUnownedResult<fuchsia_hardware_platform_bus::SysSuspend::Callback>&
                      status) {
                if (!status.ok()) {
                  device_suspend_reply(sys_root, status.status(), DEV_POWER_STATE_D0);
                  return;
                }
                device_suspend_reply(sys_root, status->out_status, status->out_state);
              });
      return;
    }
  }
  device_suspend_reply(p->sys_root, ZX_OK, 0);
}

static void sys_device_child_pre_release(void* ctx, void* child_ctx) {
  auto* p = reinterpret_cast<sysdev_suspend_t*>(ctx);
  if (child_ctx == p->pbus_instance) {
    p->pbus_instance = nullptr;
  }
}

static void sys_device_release(void* ctx) {
  auto* p = reinterpret_cast<sysdev_suspend_t*>(ctx);
  delete p;
}

static zx_protocol_device_t sys_device_proto = []() {
  zx_protocol_device_t result = {};

  result.version = DEVICE_OPS_VERSION;
  result.suspend = sys_device_suspend;
  result.child_pre_release = sys_device_child_pre_release;
  result.release = sys_device_release;
  return result;
}();

zx_status_t PlatformBus::Create(zx_device_t* parent, const char* name, zx::channel items_svc) {
  // This creates the "sys" device.
  sys_device_proto.version = DEVICE_OPS_VERSION;

  // The suspend op needs to get access to the PBus instance, to be able to
  // callback the ACPI suspend hook. Introducing a level of indirection here
  // to allow us to update the PBus instance in the device context after creating
  // the device.
  fbl::AllocChecker ac;
  std::unique_ptr<sysdev_suspend_t> suspend(new (&ac) sysdev_suspend_t);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  suspend->pbus_instance = nullptr;

  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = "sys",
      .ctx = suspend.get(),
      .ops = &sys_device_proto,
      .flags = DEVICE_ADD_NON_BINDABLE,
  };

  // Create /dev/sys.
  if (zx_status_t status = device_add(parent, &args, &suspend->sys_root); status != ZX_OK) {
    return status;
  }
  sysdev_suspend_t* suspend_ptr = suspend.release();

  // Add child of sys for the board driver to bind to.
  std::unique_ptr<platform_bus::PlatformBus> bus(
      new (&ac) platform_bus::PlatformBus(suspend_ptr->sys_root, std::move(items_svc)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (zx_status_t status = bus->Init(); status != ZX_OK) {
    zxlogf(ERROR, "failed to init: %s", zx_status_get_string(status));
    return status;
  }
  // devmgr is now in charge of the device.
  platform_bus::PlatformBus* bus_ptr = bus.release();
  suspend_ptr->pbus_instance = bus_ptr;

  return ZX_OK;
}

PlatformBus::PlatformBus(zx_device_t* parent, zx::channel items_svc)
    : PlatformBusType(parent),
      items_svc_(fidl::ClientEnd<fuchsia_boot::Items>(std::move(items_svc))),
      outgoing_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {}

zx::result<zbi_board_info_t> PlatformBus::GetBoardInfo() {
  zx::result result = GetBootItem(ZBI_TYPE_DRV_BOARD_INFO, 0);
  if (result.is_error()) {
    // This is expected on some boards.
    zxlogf(INFO, "Boot Item ZBI_TYPE_DRV_BOARD_INFO not found");
    return result.take_error();
  }
  auto& [vmo, length] = *result;
  if (length != sizeof(zbi_board_info_t)) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  zbi_board_info_t board_info;
  zx_status_t status = vmo.read(&board_info, 0, length);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to read zbi_board_info_t VMO");
    return zx::error(status);
  }
  return zx::ok(board_info);
}

zx_status_t PlatformBus::Init() {
  zx_status_t status;
  // Set up a dummy IOMMU protocol to use in the case where our board driver
  // does not set a real one.
  zx_iommu_desc_dummy_t desc;
  zx::unowned_resource iommu_resource(get_iommu_resource(parent()));
  if (iommu_resource->is_valid()) {
    status = zx::iommu::create(*iommu_resource, ZX_IOMMU_TYPE_DUMMY, &desc, sizeof(desc),
                               &iommu_handle_);
    if (status != ZX_OK) {
      return status;
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
      return boot_item.status_value();
    }
    if (boot_item.is_ok()) {
      interrupt_controller_type_ = controller;
    }
  }
#endif

  // Read platform ID.
  zx::result platform_id_result = GetBootItem(ZBI_TYPE_PLATFORM_ID, 0);
  if (platform_id_result.is_error() && platform_id_result.status_value() != ZX_ERR_NOT_FOUND) {
    return platform_id_result.status_value();
  }

#if __aarch64__
  {
    // For arm64, we do not expect a board to set the bootloader info.
    fbl::AutoLock lock(&bootloader_info_lock_);
    bootloader_info_.vendor() = "<unknown>";
  }
#endif

  fbl::AutoLock lock(&board_info_lock_);
  if (platform_id_result.is_ok()) {
    if (platform_id_result->length != sizeof(zbi_platform_id_t)) {
      return ZX_ERR_INTERNAL;
    }
    zbi_platform_id_t platform_id;
    status = platform_id_result->vmo.read(&platform_id, 0, sizeof(platform_id));
    if (status != ZX_OK) {
      return status;
    }
    zxlogf(INFO, "platform bus: VID: %u PID: %u board: \"%s\"", platform_id.vid, platform_id.pid,
           platform_id.board_name);
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
    zxlogf(ERROR, "platform_bus: ZBI_TYPE_PLATFORM_ID not found");
    return ZX_ERR_INTERNAL;
#endif
  }

  // Set default board_revision.
  zx::result zbi_board_info = GetBoardInfo();
  if (zbi_board_info.is_ok()) {
    board_info_.board_revision() = zbi_board_info->revision;
  }

  // Then we attach the platform-bus device below it.
  status = DdkAdd(ddk::DeviceAddArgs("platform").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

  zx_device_prop_t passthrough_props[] = {
      {BIND_PLATFORM_DEV_VID, 0, board_info_.vid()},
      {BIND_PLATFORM_DEV_PID, 0, board_info_.pid()},
  };
  status = AddProtocolPassthrough("pt", passthrough_props, this, &protocol_passthrough_);
  if (status != ZX_OK) {
    // We log the error but we do nothing as we've already added the device successfully.
    zxlogf(ERROR, "Error while adding pt: %s", zx_status_get_string(status));
  }
  return ZX_OK;
}

zx::result<> PlatformBus::ValidateResources(fuchsia_hardware_platform_bus::Node& node) {
  if (node.name() == std::nullopt) {
    zxlogf(ERROR, "Node has no name?");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (node.mmio() != std::nullopt) {
    for (size_t i = 0; i < node.mmio()->size(); i++) {
      if (!IsValid(node.mmio().value()[i])) {
        zxlogf(ERROR, "node '%s' has invalid mmio %zu", node.name()->data(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.irq() != std::nullopt) {
    for (size_t i = 0; i < node.irq()->size(); i++) {
      if (!IsValid(node.irq().value()[i])) {
        zxlogf(ERROR, "node '%s' has invalid irq %zu", node.name()->data(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.bti() != std::nullopt) {
    for (size_t i = 0; i < node.bti()->size(); i++) {
      if (!IsValid(node.bti().value()[i])) {
        zxlogf(ERROR, "node '%s' has invalid bti %zu", node.name()->data(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.smc() != std::nullopt) {
    for (size_t i = 0; i < node.smc()->size(); i++) {
      if (!IsValid(node.smc().value()[i])) {
        zxlogf(ERROR, "node '%s' has invalid smc %zu", node.name()->data(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.metadata() != std::nullopt) {
    for (size_t i = 0; i < node.metadata()->size(); i++) {
      if (!IsValid(node.metadata().value()[i])) {
        zxlogf(ERROR, "node '%s' has invalid metadata %zu", node.name()->data(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  if (node.boot_metadata() != std::nullopt) {
    for (size_t i = 0; i < node.boot_metadata()->size(); i++) {
      if (!IsValid(node.boot_metadata().value()[i])) {
        zxlogf(ERROR, "node '%s' has invalid boot metadata %zu", node.name()->data(), i);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
  }
  return zx::ok();
}

void PlatformBus::DdkInit(ddk::InitTxn txn) {
  zx::result board_data = GetBootItemArray(ZBI_TYPE_DRV_BOARD_PRIVATE, 0);
  if (board_data.is_error() && board_data.status_value() != ZX_ERR_NOT_FOUND) {
    return txn.Reply(board_data.status_value());
  }
  if (board_data.is_ok()) {
    zx_status_t status = device_add_metadata(protocol_passthrough_, DEVICE_METADATA_BOARD_PRIVATE,
                                             board_data->data(), board_data->size());
    if (status != ZX_OK) {
      return txn.Reply(status);
    }
  }
  fuchsia_hardware_platform_bus::Node device = {};
  device.name() = "ram-disk";
  device.vid() = PDEV_VID_GENERIC;
  device.pid() = PDEV_PID_GENERIC;
  device.did() = PDEV_DID_RAM_DISK;
  auto status = NodeAddInternal(device);
  if (status.is_error()) {
    return txn.Reply(status.error_value());
  }
  device.name() = "ram-nand";
  device.vid() = PDEV_VID_GENERIC;
  device.pid() = PDEV_PID_GENERIC;
  device.did() = PDEV_DID_RAM_NAND;
  status = NodeAddInternal(device);
  if (status.is_error()) {
    return txn.Reply(status.error_value());
  }
  device.name() = "virtual-audio";
  device.vid() = PDEV_VID_GENERIC;
  device.pid() = PDEV_PID_GENERIC;
  device.did() = PDEV_DID_VIRTUAL_AUDIO;
  status = NodeAddInternal(device);
  if (status.is_error()) {
    return txn.Reply(status.error_value());
  }
  device.name() = "bt-hci-emulator";
  device.vid() = PDEV_VID_GENERIC;
  device.pid() = PDEV_PID_GENERIC;
  device.did() = PDEV_DID_BT_HCI_EMULATOR;
  status = NodeAddInternal(device);
  if (status.is_error()) {
    return txn.Reply(status.error_value());
  }

  device.name() = "fake-battery";
  device.vid() = PDEV_VID_GENERIC;
  device.pid() = PDEV_PID_GENERIC;
  device.did() = PDEV_DID_FAKE_BATTERY;
  status = NodeAddInternal(device);
  if (status.is_error()) {
    return txn.Reply(status.error_value());
  }
  device.name() = "fake-ac";
  device.vid() = PDEV_VID_GENERIC;
  device.pid() = PDEV_PID_GENERIC;
  device.did() = PDEV_DID_FAKE_AC;
  status = NodeAddInternal(device);
  if (status.is_error()) {
    return txn.Reply(status.error_value());
  }

  return txn.Reply(ZX_OK);  // This will make the device visible and able to be unbound.
}

zx_status_t platform_bus_create(void* ctx, zx_device_t* parent, const char* name,
                                zx_handle_t handle) {
  return platform_bus::PlatformBus::Create(parent, name, zx::channel(handle));
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.create = platform_bus_create;
  return ops;
}();

}  // namespace platform_bus

ZIRCON_DRIVER(platform_bus, platform_bus::driver_ops, "zircon", "0.1");
