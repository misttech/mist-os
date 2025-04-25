// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vendor/misttech/src/devices/bus/drivers/platform/platform-bus.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <stdlib.h>
#include <trace.h>
#include <zircon/status.h>
#include <zircon/syscalls/iommu.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <object/bus_transaction_initiator_dispatcher.h>

#define LOCAL_TRACE 0

namespace platform_bus {

namespace {

// Adds a passthrough device which forwards all banjo connections to the parent device.
// The device will be added as a child of |parent| with the name |name|, and |props| will
// be applied to the new device's add_args.
// Returns ZX_OK if the device is successfully added.
zx_status_t AddProtocolPassthrough(const char* name, cpp20::span<const zx_device_str_prop_t> props,
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

#if 0
  fhpb::Service::InstanceHandler handler({
      .platform_bus = parent->bindings().CreateHandler(parent, fdf::Dispatcher::GetCurrent()->get(),
                                                       fidl::kIgnoreBindingClosure),
      .iommu = parent->iommu_bindings().CreateHandler(parent, fdf::Dispatcher::GetCurrent()->get(),
                                                      fidl::kIgnoreBindingClosure),
      .firmware = parent->fw_bindings().CreateHandler(parent, fdf::Dispatcher::GetCurrent()->get(),
                                                      fidl::kIgnoreBindingClosure),
  });

  auto status = parent->outgoing().AddService<fhpb::Service>(std::move(handler));
  if (status.is_error()) {
    return status.error_value();
  }

  status = parent->outgoing().AddService<fuchsia_sysinfo::Service>(
      fuchsia_sysinfo::Service::InstanceHandler({
          .device = parent->sysinfo_bindings().CreateHandler(
              parent, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
              fidl::kIgnoreBindingClosure),
      }));
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
      fhpb::Service::Name,
      fuchsia_sysinfo::Service::Name,
  };
#endif

  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = name,
      .ctx = parent,
      .ops = &passthrough_proto,
      .str_props = props.data(),
      .str_prop_count = static_cast<uint32_t>(props.size()),
      //.runtime_service_offers = offers.data(),
      //.runtime_service_offer_count = offers.size(),
      //.outgoing_dir_channel = endpoints->client.TakeChannel().release(),
  };

  return device_add(parent->zxdev(), &args, out_device);
}

}  // namespace

zx_status_t PlatformBus::IommuGetBti(uint32_t iommu_index, uint32_t bti_id,
                                     fbl::RefPtr<BusTransactionInitiatorDispatcher>* out_bti) {
  if (iommu_index != 0) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  std::pair key(iommu_index, bti_id);
  auto bti = cached_btis_.find(key);
  if (bti == cached_btis_.end()) {
    KernelHandle<BusTransactionInitiatorDispatcher> new_bti;
    zx_rights_t rights;
    zx_status_t status = BusTransactionInitiatorDispatcher::Create(iommu_handle_.dispatcher(),
                                                                   bti_id, &new_bti, &rights);
    if (status != ZX_OK) {
      return status;
    }

    auto [iter, _] = cached_btis_.emplace(key, std::move(new_bti));
    bti = iter;
  }

  *out_bti = bti->second.dispatcher();

  return ZX_OK;
}

#if 0
zx_status_t PlatformBus::Create(const char* name, PlatformBus** out_platform_bus) {
  fbl::AllocChecker ac;
  platform_bus::PlatformBus* bus = new (&ac) platform_bus::PlatformBus();
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (zx::result<> status = bus->Start(); status.is_error()) {
    LTRACEF("failed to init: %d", status.error_value());
    return status.error_value();
  }

  *out_platform_bus = bus;

  return ZX_OK;
}
#endif

zx_status_t PlatformBus::GetBti(uint32_t iommu_index, uint32_t bti_id,
                                fbl::RefPtr<BusTransactionInitiatorDispatcher>* out_bti) {
  fbl::RefPtr<BusTransactionInitiatorDispatcher> bti;
  zx_status_t status = IommuGetBti(iommu_index, bti_id, &bti);

  if (status != ZX_OK) {
    return ZX_ERR_NOT_FOUND;
  }

  *out_bti = std::move(bti);
  return ZX_OK;
}

zx::result<fbl::Vector<PlatformBus::BootItemResult>> PlatformBus::GetBootItem(
    uint32_t type, std::optional<uint32_t> extra) {
  // fbl::Vector<PlatformBus::BootItemResult> ret;
  //  ret.reserve(items.count());
  // return zx::ok(std::move(ret));
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<fbl::Array<uint8_t>> PlatformBus::GetBootItemArray(uint32_t type,
                                                              std::optional<uint32_t> extra) {
  zx::result result = GetBootItem(type, extra);
  if (result.is_error()) {
    return result.take_error();
  }
  if (result->size() > 1) {
    LTRACEF("Found multiple boot items of type: %u\n", type);
  }
  auto& [vmo, length] = result.value()[0];
  fbl::AllocChecker ac;
  fbl::Array<uint8_t> data(new (&ac) uint8_t[length], length);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  zx_status_t status = vmo->Read(data.data(), 0, data.size());
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
#if 0
    auto& suspend_cb = pbus->suspend_cb();
    if (suspend_cb.is_valid()) {
      suspend_cb->Callback(enable_wake, suspend_reason)
          .ThenExactlyOnce([sys_root = p->sys_root](
                               fidl::WireUnownedResult<fhpb::SysSuspend::Callback>& status) {
            if (!status.ok()) {
              device_suspend_reply(sys_root, status.status(), DEV_POWER_STATE_D0);
              return;
            }
            device_suspend_reply(sys_root, status->out_status, DEV_POWER_STATE_D0);
          });
      return;
    }
#endif
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

zx_status_t PlatformBus::Create(zx_device_t* parent, const char* name) {
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
      new (&ac) platform_bus::PlatformBus(suspend_ptr->sys_root /*, std::move(items_svc)*/));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (zx_status_t status = bus->Init(); status != ZX_OK) {
    LTRACEF("failed to init: %s", zx_status_get_string(status));
    return status;
  }
  // devmgr is now in charge of the device.
  platform_bus::PlatformBus* bus_ptr = bus.release();
  suspend_ptr->pbus_instance = bus_ptr;

  return ZX_OK;
}

PlatformBus::PlatformBus(zx_device_t* parent) : PlatformBusType(parent) {}

zx::result<zbi_board_info_t> PlatformBus::GetBoardInfo() {
  zx::result result = GetBootItem(ZBI_TYPE_DRV_BOARD_INFO, {});
  if (result.is_error()) {
    // This is expected on some boards.
    dprintf(INFO, "Boot Item ZBI_TYPE_DRV_BOARD_INFO not found\n");
    return result.take_error();
  }
  auto& [vmo, length] = result.value()[0];
  if (length != sizeof(zbi_board_info_t)) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  zbi_board_info_t board_info;
  zx_status_t status = vmo->Read(&board_info, 0, length);
  if (status != ZX_OK) {
    dprintf(CRITICAL, "Failed to read zbi_board_info_t VMO: %s\n", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(board_info);
}

zx_status_t PlatformBus::Init() {
  zx_status_t status;
  // Set up a dummy IOMMU protocol to use in the case where our board driver
  // does not set a real one.
  zx_iommu_desc_dummy_t desc;

  zx_rights_t rights;
  size_t desc_size = sizeof(desc);
  fbl::AllocChecker ac;
  ktl::unique_ptr<uint8_t[]> copied_desc(new (&ac) uint8_t[desc_size]);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  memcpy(copied_desc.get(), &desc, desc_size);
  status = IommuDispatcher::Create(ZX_IOMMU_TYPE_DUMMY, std::move(copied_desc), desc_size,
                                   &iommu_handle_, &rights);
  if (status != ZX_OK) {
    return status;
  }

  // Read kernel driver.
#if __x86_64__
  // interrupt_controller_type_ = fuchsia_sysinfo::wire::InterruptControllerType::kApic;
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
    return platform_id_result.status_value();
  }

#if __aarch64__
  {
    // For arm64, we do not expect a board to set the bootloader info.
    bootloader_info_.vendor() = "<unknown>";
  }
#endif

  if (platform_id_result.is_ok()) {
    if (platform_id_result.value()[0].length != sizeof(zbi_platform_id_t)) {
      return ZX_ERR_INTERNAL;
    }
    zbi_platform_id_t platform_id;
    zx_status_t s = platform_id_result.value()[0].vmo->Read(&platform_id, 0, sizeof(platform_id));
    if (s != ZX_OK) {
      return s;
    }
    dprintf(INFO, "VID: %u PID: %u board: \"%s\"\n", platform_id.vid, platform_id.pid,
            platform_id.board_name);
    //  fdf::info("VID: {} PID: {} board: \"{}\"", platform_id.vid, platform_id.pid,
    //            std::string_view(platform_id.board_name));
    //  board_info_.vid() = platform_id.vid;
    //  board_info_.pid() = platform_id.pid;
    //  board_info_.board_name() = platform_id.board_name;
  } else {
#if __x86_64__
    // For x64, we might not find the ZBI_TYPE_PLATFORM_ID, old bootloaders
    // won't support this, for example. If this is the case, cons up the VID/PID
    // here to allow the acpi board driver to load and bind.
    // board_info_.vid() = PDEV_VID_INTEL;
    // board_info_.pid() = PDEV_PID_X86;
#else
    dprintf(ERROR, "ZBI_TYPE_PLATFORM_ID not found");
    return ZX_ERR_INTERNAL;
#endif
  }

  // Set default board_revision.
  zx::result zbi_board_info = GetBoardInfo();
  if (zbi_board_info.is_ok()) {
    // board_info_.board_revision() = zbi_board_info->revision;
  }

  // Then we attach the platform-bus device below it.
  status = DdkAdd(ddk::DeviceAddArgs("platform").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

#if 0
  zx::result board_data = GetBootItemArray(ZBI_TYPE_DRV_BOARD_PRIVATE, {});
  if (board_data.is_error() && board_data.status_value() != ZX_ERR_NOT_FOUND) {
    return board_data.status_value();
  }
#endif

  zx_device_str_prop_t passthrough_props[] = {
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID, static_cast<uint32_t>(PDEV_VID_INTEL)),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID, static_cast<uint32_t>(PDEV_PID_X86)),
  };
  status = AddProtocolPassthrough("pt", passthrough_props, this, &protocol_passthrough_);
  if (status != ZX_OK) {
    // We log the error but we do nothing as we've already added the device successfully.
    LTRACEF("Error while adding pt: %s\n", zx_status_get_string(status));
  }

  return ZX_OK;
}

void PlatformBus::DdkInit(ddk::InitTxn txn) {
#if 0
  zx::result board_data = GetBootItemArray(ZBI_TYPE_DRV_BOARD_PRIVATE, {});
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
  zx::vmo config_vmo;
  {
    zx_status_t status = device_get_config_vmo(zxdev(), config_vmo.reset_and_get_address());
    if (status != ZX_OK) {
      return txn.Reply(status);
    }
  }

  auto config = platform_bus_config::Config::CreateFromVmo(std::move(config_vmo));
  if (config.software_device_ids().size() != config.software_device_names().size()) {
    zxlogf(ERROR,
           "Invalid config. software_device_ids and software_device_names must have same length");
    return txn.Reply(ZX_ERR_INVALID_ARGS);
  }
  for (size_t i = 0; i < config.software_device_ids().size(); i++) {
    fhpb::Node device = {};
    device.name() = config.software_device_names()[i];
    device.vid() = PDEV_VID_GENERIC;
    device.pid() = PDEV_PID_GENERIC;
    device.did() = config.software_device_ids()[i];
    auto status = NodeAddInternal(device);
    if (status.is_error()) {
      return txn.Reply(status.error_value());
    }
  }

  suspend_enabled_ = config.suspend_enabled();
#endif

  return txn.Reply(ZX_OK);  // This will make the device visible and able to be unbound.
}

zx_status_t platform_bus_create(void* ctx, zx_device_t* parent, const char* name) {
  return platform_bus::PlatformBus::Create(parent, name);
}

static zx_driver_ops_t platform_bus_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.create = platform_bus_create;
  return ops;
}();

}  // namespace platform_bus

MISTOS_DRIVER(platform_bus, platform_bus::platform_bus_driver_ops, "mistos", "0.1", 1)
// loaded by devcoordinator
BI_ABORT_IF_AUTOBIND, MISTOS_DRIVER_END(platform_bus)
