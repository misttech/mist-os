// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>
#include <limits.h>

#include <acpica/acpi.h>

#include "acpi-private.h"
#include "dev.h"
#include "errors.h"
#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/util.h"
#include "x86.h"

namespace x86 {

bool use_hardware_iommu(zx_device_t* dev) {
  char value[32];
  auto status = device_get_variable(dev, "driver.iommu.enable", value, sizeof(value), nullptr);
  if (status != ZX_OK) {
    return false;  // Default to false currently
  } else if (!strcmp(value, "0") || !strcmp(value, "false") || !strcmp(value, "off")) {
    return false;
  } else {
    return true;
  }
}

zx_status_t X86::EarlyAcpiInit() {
  ZX_DEBUG_ASSERT(!acpica_initialized_);
  // First initialize the ACPI subsystem.
  zx_status_t status = acpi_->InitializeAcpi().zx_status_value();
  if (status != ZX_OK) {
    return status;
  }
  acpica_initialized_ = true;
  return ZX_OK;
}

zx_status_t X86::EarlyInit() {
  zx_status_t status = EarlyAcpiInit();
  if (status != ZX_OK) {
    return status;
  }
  // Please do not use get_root_resource() in new code. See fxbug.dev/31358.
  zx::unowned_resource root_resource(get_root_resource(parent()));
  // Now initialize the IOMMU manager. Any failures in setting it up we consider non-fatal and do
  // not propagate.
  status = iommu_manager_.Init(std::move(root_resource), use_hardware_iommu(parent()));
  if (status != ZX_OK) {
    zxlogf(INFO, "acpi: Failed to initialize IOMMU manager: %s", zx_status_get_string(status));
  }
  return ZX_OK;
}

}  // namespace x86
