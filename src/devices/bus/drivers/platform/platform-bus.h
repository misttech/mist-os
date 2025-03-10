// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_BUS_H_
#define SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_BUS_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/device.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/sync/completion.h>
#include <lib/zbi-format/board.h>
#include <lib/zx/channel.h>
#include <lib/zx/iommu.h>
#include <lib/zx/resource.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <fbl/array.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <sdk/lib/component/outgoing/cpp/outgoing_directory.h>

#include "platform-device.h"

namespace platform_bus {

class PlatformBus;
using PlatformBusType = ddk::Device<PlatformBus, ddk::Initializable>;

// This is the main class for the platform bus driver.
class PlatformBus : public PlatformBusType,
                    public fdf::WireServer<fuchsia_hardware_platform_bus::PlatformBus>,
                    public fdf::WireServer<fuchsia_hardware_platform_bus::Iommu>,
                    public fdf::WireServer<fuchsia_hardware_platform_bus::Firmware>,
                    public fidl::WireServer<fuchsia_sysinfo::SysInfo> {
 public:
  static zx_status_t Create(zx_device_t* parent, const char* name, zx::channel items_svc);

  PlatformBus(zx_device_t* parent, zx::channel items_svc);

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // fuchsia.hardware.platform.bus.PlatformBus implementation.
  void NodeAdd(NodeAddRequestView request, fdf::Arena& arena,
               NodeAddCompleter::Sync& completer) override;

  void GetBoardInfo(fdf::Arena& arena, GetBoardInfoCompleter::Sync& completer) override;
  void SetBoardInfo(SetBoardInfoRequestView request, fdf::Arena& arena,
                    SetBoardInfoCompleter::Sync& completer) override;
  void SetBootloaderInfo(SetBootloaderInfoRequestView request, fdf::Arena& arena,
                         SetBootloaderInfoCompleter::Sync& completer) override;

  void RegisterSysSuspendCallback(RegisterSysSuspendCallbackRequestView request, fdf::Arena& arena,
                                  RegisterSysSuspendCallbackCompleter::Sync& completer) override;
  void AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request, fdf::Arena& arena,
                            AddCompositeNodeSpecCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_bus::PlatformBus> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fuchsia.hardware.platform.bus.Iommu implementation.
  void GetBti(GetBtiRequestView request, fdf::Arena& arena,
              GetBtiCompleter::Sync& completer) override;

  // fuchsia.hardware.platform.bus.Firmware implementation.
  void GetFirmware(GetFirmwareRequestView request, fdf::Arena& arena,
                   GetFirmwareCompleter::Sync& completer) override;

  // SysInfo protocol implementation.
  void GetBoardName(GetBoardNameCompleter::Sync& completer) override;
  void GetBoardRevision(GetBoardRevisionCompleter::Sync& completer) override;
  void GetBootloaderVendor(GetBootloaderVendorCompleter::Sync& completer) override;
  void GetInterruptControllerInfo(GetInterruptControllerInfoCompleter::Sync& completer) override;
  void GetSerialNumber(GetSerialNumberCompleter::Sync& completer) override;

  // IOMMU protocol implementation.
  zx_status_t IommuGetBti(uint32_t iommu_index, uint32_t bti_id, zx::bti* out_bti);

  zx::unowned_resource GetIrqResource() const {
    return zx::unowned_resource(get_irq_resource(parent()));
  }

  zx::unowned_resource GetMmioResource() const {
    return zx::unowned_resource(get_mmio_resource(parent()));
  }

  zx::unowned_resource GetSmcResource() const {
    return zx::unowned_resource(get_smc_resource(parent()));
  }

  struct BootItemResult {
    zx::vmo vmo;
    uint32_t length;
  };
  // Returns ZX_ERR_NOT_FOUND when boot item wasn't found.
  zx::result<std::vector<BootItemResult>> GetBootItem(uint32_t type, std::optional<uint32_t> extra);
  zx::result<fbl::Array<uint8_t>> GetBootItemArray(uint32_t type, std::optional<uint32_t> extra);

  fidl::WireClient<fuchsia_hardware_platform_bus::SysSuspend>& suspend_cb() { return suspend_cb_; }

  fuchsia_hardware_platform_bus::TemporaryBoardInfo board_info() {
    fbl::AutoLock lock(&board_info_lock_);
    return board_info_;
  }

  fdf::OutgoingDirectory& outgoing() { return outgoing_; }

  fdf::UnownedDispatcher dispatcher() { return dispatcher_->borrow(); }

  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::PlatformBus>& bindings() {
    return bindings_;
  }

  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::Iommu>& iommu_bindings() {
    return iommu_bindings_;
  }

  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::Firmware>& fw_bindings() {
    return fw_bindings_;
  }

  fidl::ServerBindingGroup<fuchsia_sysinfo::SysInfo>& sysinfo_bindings() {
    return sysinfo_bindings_;
  }

  bool suspend_enabled() const { return suspend_enabled_; }

 private:
  fidl::WireClient<fuchsia_hardware_platform_bus::SysSuspend> suspend_cb_;

  DISALLOW_COPY_ASSIGN_AND_MOVE(PlatformBus);

  zx::result<zbi_board_info_t> GetBoardInfo();
  zx_status_t Init();

  zx::result<> NodeAddInternal(fuchsia_hardware_platform_bus::Node& node);
  zx::result<> ValidateResources(fuchsia_hardware_platform_bus::Node& node);

  fidl::ClientEnd<fuchsia_boot::Items> items_svc_;

  // Protects board_name_completer_.
  fbl::Mutex board_info_lock_;
  fuchsia_hardware_platform_bus::TemporaryBoardInfo board_info_ __TA_GUARDED(board_info_lock_) = {};
  // List to cache requests when board_name is not yet set.
  std::vector<GetBoardNameCompleter::Async> board_name_completer_ __TA_GUARDED(board_info_lock_);

  fbl::Mutex bootloader_info_lock_;
  fuchsia_hardware_platform_bus::BootloaderInfo bootloader_info_
      __TA_GUARDED(bootloader_info_lock_) = {};
  // List to cache requests when vendor is not yet set.
  std::vector<GetBootloaderVendorCompleter::Async> bootloader_vendor_completer_
      __TA_GUARDED(bootloader_info_lock_);

  fuchsia_sysinfo::wire::InterruptControllerType interrupt_controller_type_ =
      fuchsia_sysinfo::wire::InterruptControllerType::kUnknown;

  // Dummy IOMMU.
  zx::iommu iommu_handle_;

  std::map<std::pair<uint32_t, uint32_t>, zx::bti> cached_btis_;

  zx_device_t* protocol_passthrough_ = nullptr;
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::PlatformBus> bindings_;
  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::Iommu> iommu_bindings_;
  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::Firmware> fw_bindings_;
  fidl::ServerBindingGroup<fuchsia_sysinfo::SysInfo> sysinfo_bindings_;
  fdf::UnownedDispatcher dispatcher_;
  std::optional<inspect::ComponentInspector> inspector_;

  bool suspend_enabled_ = false;
};

}  // namespace platform_bus

__BEGIN_CDECLS
zx_status_t platform_bus_create(void* ctx, zx_device_t* parent, const char* name, const char* args,
                                zx_handle_t rpc_channel);
__END_CDECLS

#endif  // SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_BUS_H_
