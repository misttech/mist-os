// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_LIB_ACPI_PCI_H_
#define SRC_DEVICES_BOARD_LIB_ACPI_PCI_H_

#include <lib/ddk/device.h>
#include <mistos/hardware/pciroot/cpp/banjo.h>

#include <acpica/acpi.h>
#include <fbl/vector.h>

#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/manager.h"

zx_status_t pci_init(zx_device_t* parent, ACPI_HANDLE object,
                     acpi::UniquePtr<ACPI_DEVICE_INFO> info, acpi::Manager* acpi,
                     fbl::Vector<pci_bdf_t> acpi_bdfs);

#endif  // SRC_DEVICES_BOARD_LIB_ACPI_PCI_H_
