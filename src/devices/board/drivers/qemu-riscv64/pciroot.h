// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BOARD_DRIVERS_QEMU_RISCV64_PCIROOT_H_
#define SRC_DEVICES_BOARD_DRIVERS_QEMU_RISCV64_PCIROOT_H_

#include <fuchsia/hardware/iommu/cpp/banjo.h>
#include <fuchsia/hardware/pciroot/cpp/banjo.h>
#include <lib/pci/pciroot.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <memory>

#include <ddktl/device.h>

namespace board_qemu_riscv64 {
class QemuRiscv64Pciroot : public PcirootBase {
 public:
  struct Context {
    zx::vmo ecam;
  };
  static zx::result<> Create(PciRootHost* root_host, QemuRiscv64Pciroot::Context ctx,
                             zx_device_t* parent, const char* name);
  zx_status_t PcirootGetBti(uint32_t bdf, uint32_t index, zx::bti* bti) final;
  zx_status_t PcirootGetPciPlatformInfo(pci_platform_info_t* info) final;
  ~QemuRiscv64Pciroot() override = default;

 private:
  Context context_;
  QemuRiscv64Pciroot(PciRootHost* root_host, QemuRiscv64Pciroot::Context context,
                     zx_device_t* parent, const char* name)
      : PcirootBase(root_host, parent, name), context_(std::move(context)), iommu_(parent) {}
  ddk::IommuProtocolClient iommu_;
};

}  // namespace board_qemu_riscv64

#endif  // SRC_DEVICES_BOARD_DRIVERS_QEMU_RISCV64_PCIROOT_H_
