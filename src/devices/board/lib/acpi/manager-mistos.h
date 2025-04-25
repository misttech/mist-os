// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_LIB_ACPI_MANAGER_MISTOS_H_
#define SRC_DEVICES_BOARD_LIB_ACPI_MANAGER_MISTOS_H_

// #include <lib/async-loop/cpp/loop.h>
// #include <lib/fit/function.h>

#include "src/devices/board/lib/acpi/manager.h"

namespace acpi {

// Specialisation of ACPI manager for mist-os.
class MistosManager : public Manager {
 public:
  MistosManager(acpi::Acpi* acpi, iommu::IommuManagerInterface* iommu, zx_device_t* acpi_root)
      : Manager(acpi, iommu, acpi_root) {}
};

}  // namespace acpi

#endif  // SRC_DEVICES_BOARD_LIB_ACPI_MANAGER_MISTOS_H_
