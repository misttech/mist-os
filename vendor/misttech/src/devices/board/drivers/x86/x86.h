// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_DEVICES_BOARD_DRIVERS_X86_X86_H_
#define VENDOR_MISTTECH_DEVICES_BOARD_DRIVERS_X86_X86_H_

#include <lib/ddk/device.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <ktl/unique_ptr.h>

#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/manager.h"

namespace x86 {

class X86;
using DeviceType = ddk::Device<X86, ddk::Initializable>;

// This is the main class for the X86 platform bus driver.
class X86 : public DeviceType {
 public:
  explicit X86(zx_device_t* parent, ktl::unique_ptr<acpi::Acpi> acpi)
      : DeviceType(parent), acpi_(std::move(acpi)) {}
  ~X86();

  static zx_status_t Create(void* ctx, zx_device_t* parent, std::unique_ptr<X86>* out);
  static zx_status_t CreateAndBind(void* ctx, zx_device_t* parent);
  // static bool RunUnitTests(void* ctx, zx_device_t* parent, zx_handle_t channel);

  // Device protocol implementation.
  void DdkRelease();
  void DdkInit(ddk::InitTxn txn);

  // Performs ACPICA initialization.
  zx_status_t EarlyAcpiInit();

  zx_status_t EarlyInit();

  // Add the list of ACPI entries present in the system to |entries|.
  //
  // Requires that ACPI has been initialised.
  // zx_status_t GetAcpiTableEntries(fbl::Vector<fuchsia_acpi_tables::wire::TableInfo>* entries);

  acpi::Acpi* acpi() { return acpi_.get(); }

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

  // IommuManager iommu_manager_{
  //     [](fx_log_severity_t severity, const char* file, int line, const char* msg, va_list args) {
  //       zxlogvf_etc(severity, nullptr, file, line, msg, args);
  //     }};

  ktl::unique_ptr<acpi::Manager> acpi_manager_;
  ktl::unique_ptr<acpi::Acpi> acpi_;
  // Whether the global ACPICA initialization has been performed or not
  bool acpica_initialized_ = false;
};

}  // namespace x86

#endif  // VENDOR_MISTTECH_DEVICES_BOARD_DRIVERS_X86_X86_H_
