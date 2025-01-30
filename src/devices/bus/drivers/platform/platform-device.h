// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_DEVICE_H_
#define SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_DEVICE_H_

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/zx/bti.h>
#include <lib/zx/channel.h>

#include <ddktl/device.h>
#include <fbl/vector.h>

#include "src/devices/bus/drivers/platform/platform-interrupt.h"

namespace platform_bus {

class PlatformBus;

// Restricted version of the platform bus protocol that does not allow devices to be added.
class RestrictPlatformBus : public fdf::WireServer<fuchsia_hardware_platform_bus::PlatformBus> {
 public:
  RestrictPlatformBus(PlatformBus* upstream) : upstream_(upstream) {}
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

 private:
  PlatformBus* upstream_;
};

class PlatformDevice;
using PlatformDeviceType = ddk::Device<PlatformDevice, ddk::GetProtocolable, ddk::Initializable>;

// This class represents a platform device attached to the platform bus.
// Instances of this class are created by PlatformBus at boot time when the board driver
// calls the platform bus protocol method pbus_device_add().

class PlatformDevice : public PlatformDeviceType,
                       public ddk::PDevProtocol<PlatformDevice, ddk::base_protocol>,
                       public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  enum Type {
    // This platform device is started in a new devhost.
    Isolated,
    // This platform device is run in the same process as platform bus and provides
    // its protocol to the platform bus.
    Protocol,
    // This platform device is a fragment for a composite device.
    Fragment,
  };

  // Creates a new PlatformDevice instance.
  // *flags* contains zero or more PDEV_ADD_* flags from the platform bus protocol.
  static zx_status_t Create(fuchsia_hardware_platform_bus::Node node, zx_device_t* parent,
                            PlatformBus* bus, Type type, inspect::ComponentInspector& inspector,
                            std::unique_ptr<platform_bus::PlatformDevice>* out);

  inline uint32_t vid() const { return vid_; }
  inline uint32_t pid() const { return pid_; }
  inline uint32_t did() const { return did_; }
  inline uint32_t instance_id() const { return instance_id_; }

  // Device protocol implementation.
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);
  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // Platform device protocol implementation, for devices that run in-process.
  zx_status_t PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio);
  zx_status_t PDevGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq);
  zx_status_t PDevGetBti(uint32_t index, zx::bti* out_handle);
  zx_status_t PDevGetSmc(uint32_t index, zx::resource* out_resource);
  zx_status_t PDevGetDeviceInfo(pdev_device_info_t* out_info);
  zx_status_t PDevGetBoardInfo(pdev_board_info_t* out_info);
  zx_status_t PDevDeviceAdd(uint32_t index, const device_add_args_t* args, zx_device_t** device);

  // Platform device protocol FIDL implementation.
  void GetMmioById(GetMmioByIdRequestView request, GetMmioByIdCompleter::Sync& completer) override;
  void GetMmioByName(GetMmioByNameRequestView request,
                     GetMmioByNameCompleter::Sync& completer) override;
  void GetInterruptById(GetInterruptByIdRequestView request,
                        GetInterruptByIdCompleter::Sync& completer) override;
  void GetInterruptByName(GetInterruptByNameRequestView request,
                          GetInterruptByNameCompleter::Sync& completer) override;
  void GetBtiById(GetBtiByIdRequestView request, GetBtiByIdCompleter::Sync& completer) override;
  void GetBtiByName(GetBtiByNameRequestView request,
                    GetBtiByNameCompleter::Sync& completer) override;
  void GetSmcById(GetSmcByIdRequestView request, GetSmcByIdCompleter::Sync& completer) override;
  void GetSmcByName(GetSmcByNameRequestView request,
                    GetSmcByNameCompleter::Sync& completer) override;
  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override;
  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override;
  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override;
  void GetMetadata(GetMetadataRequestView request, GetMetadataCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // Starts the underlying devmgr device.
  zx_status_t Start();

 private:
  // Allows for `std::string_view`'s to be used when searching an unordered map that uses
  // `std::string` as its key.
  struct MetadataIdHash {
    using hash_type = std::hash<std::string_view>;
    using is_transparent = void;

    std::size_t operator()(const char* str) const { return hash_type{}(str); }
    std::size_t operator()(std::string_view str) const { return hash_type{}(str); }
    std::size_t operator()(std::string const& str) const { return hash_type{}(str); }
  };

  // *flags* contains zero or more PDEV_ADD_* flags from the platform bus protocol.
  explicit PlatformDevice(zx_device_t* parent, PlatformBus* bus, Type type,
                          inspect::Node inspect_node, fuchsia_hardware_platform_bus::Node node);
  zx_status_t Init();

  zx_status_t CreateInterruptFragments();

  fpromise::promise<inspect::Inspector> InspectNodeCallback() const;

  PlatformBus* bus_;
  char name_[ZX_DEVICE_NAME_MAX + 1];
  Type type_;
  const uint32_t vid_;
  const uint32_t pid_;
  const uint32_t did_;
  const uint32_t instance_id_;

  fuchsia_hardware_platform_bus::Node node_;
  std::unique_ptr<RestrictPlatformBus> restricted_;
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_platform_bus::PlatformBus> bus_bindings_;
  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> device_bindings_;
  std::unordered_map<std::string, std::vector<uint8_t>, MetadataIdHash, std::equal_to<>> metadata_;

  // Contains the vectors used when creating interrupts. `interrupt_vectors_`
  // must be above `inspector_`. This is to ensure that `interrupt_vectors_` is
  // not destructed before `inspector_` is destructed. When `inspector_`
  // destructs, it executes a callback that references `interrupt_vectors_`.
  std::vector<unsigned int> interrupt_vectors_;

  inspect::Node inspect_node_;
};

}  // namespace platform_bus

#endif  // SRC_DEVICES_BUS_DRIVERS_PLATFORM_PLATFORM_DEVICE_H_
