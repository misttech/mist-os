// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fuchsia/hardware/pciroot/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pci.h>

#include <ddktl/device.h>

#ifndef SRC_DEVICES_BUS_DRIVERS_PCI_KPCI_H_
#define SRC_DEVICES_BUS_DRIVERS_PCI_KPCI_H_
struct kpci_device {
  zx_device_t* zxdev;
  // only set for non-proxy devices
  pciroot_protocol_t pciroot;
  pdev_protocol_t pdev;
  // kernel pci handle, only set for shadow devices
  zx_handle_t handle;
  // nth device index
  uint32_t index;
  pci_device_info_t info;
  char name[ZX_DEVICE_NAME_MAX];
};

namespace pci {

class KernelPci;
using KernelPciType = ddk::Device<pci::KernelPci>;
class KernelPci : public KernelPciType, public fidl::WireServer<fuchsia_hardware_pci::Device> {
 public:
  KernelPci(zx_device_t* parent, kpci_device device, async_dispatcher_t* dispatcher);

  void DdkRelease();

  static zx_status_t CreateComposite(zx_device_t* parent, kpci_device device, bool uses_acpi);
  // Pci Protocol
  void GetBar(GetBarRequestView request, GetBarCompleter::Sync& completer) override;
  void SetBusMastering(SetBusMasteringRequestView request,
                       SetBusMasteringCompleter::Sync& completer) override;
  void ResetDevice(ResetDeviceCompleter::Sync& completer) override;
  void AckInterrupt(AckInterruptCompleter::Sync& completer) override;
  void MapInterrupt(MapInterruptRequestView request,
                    MapInterruptCompleter::Sync& completer) override;
  void GetInterruptModes(GetInterruptModesCompleter::Sync& completer) override;
  void SetInterruptMode(SetInterruptModeRequestView request,
                        SetInterruptModeCompleter::Sync& completer) override;
  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override;
  void ReadConfig8(ReadConfig8RequestView request, ReadConfig8Completer::Sync& completer) override;
  void ReadConfig16(ReadConfig16RequestView request,
                    ReadConfig16Completer::Sync& completer) override;
  void ReadConfig32(ReadConfig32RequestView request,
                    ReadConfig32Completer::Sync& completer) override;
  void WriteConfig8(WriteConfig8RequestView request,
                    WriteConfig8Completer::Sync& completer) override;
  void WriteConfig16(WriteConfig16RequestView request,
                     WriteConfig16Completer::Sync& completer) override;
  void WriteConfig32(WriteConfig32RequestView request,
                     WriteConfig32Completer::Sync& completer) override;
  void GetCapabilities(GetCapabilitiesRequestView request,
                       GetCapabilitiesCompleter::Sync& completer) override;
  void GetExtendedCapabilities(GetExtendedCapabilitiesRequestView request,
                               GetExtendedCapabilitiesCompleter::Sync& completer) override;
  void GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) override;

  zx_status_t SetUpOutgoingDirectory(fidl::ServerEnd<fuchsia_io::Directory> sever_end);

 private:
  kpci_device device_;
  fidl::ServerBindingGroup<fuchsia_hardware_pci::Device> bindings_;
  async_dispatcher_t* dispatcher_;
  component::OutgoingDirectory outgoing_;
};

}  // namespace pci

#endif  // SRC_DEVICES_BUS_DRIVERS_PCI_KPCI_H_
