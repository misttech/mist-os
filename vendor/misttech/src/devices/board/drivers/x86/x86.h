// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_DEVICES_BOARD_DRIVERS_X86_X86_H_
#define VENDOR_MISTTECH_DEVICES_BOARD_DRIVERS_X86_X86_H_

#include <zircon/types.h>

#include <ktl/unique_ptr.h>

#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/manager.h"

namespace x86 {

// This is the main class for the X86 platform bus driver.
class X86 {
 public:
  explicit X86(ktl::unique_ptr<acpi::Acpi> acpi) : acpi_(std::move(acpi)) {}
  ~X86();
  static zx_status_t Create(void* ctx, ktl::unique_ptr<X86>* out);
  static zx_status_t CreateAndBind(void* ctx);

  // Performs ACPICA initialization.
  zx_status_t EarlyAcpiInit();

  zx_status_t EarlyInit();

 private:
  X86(const X86&) = delete;
  X86(X86&&) = delete;
  X86& operator=(const X86&) = delete;
  X86& operator=(X86&&) = delete;

  // zx_status_t GoldfishControlInit();

  // Register this instance with devmgr.
  zx_status_t Bind();
  // Asynchronously complete initialisation.
  zx_status_t DoInit();

  ktl::unique_ptr<acpi::Manager> acpi_manager_;
  ktl::unique_ptr<acpi::Acpi> acpi_;
  // Whether the global ACPICA initialization has been performed or not
  bool acpica_initialized_ = false;
};

}  // namespace x86

#endif  // VENDOR_MISTTECH_DEVICES_BOARD_DRIVERS_X86_X86_H_
