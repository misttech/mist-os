// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vendor/misttech/src/devices/board/drivers/x86/x86.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/driver.h>
#include <trace.h>

#include <acpica/acpi.h>
#include <lk/init.h>
#include <phys/handoff.h>

#include "vendor/misttech/src/devices/board/drivers/x86/include/acpi.h"

#define LOCAL_TRACE 0

zx_paddr_t acpi_rsdp;

namespace x86 {

X86::~X86() {
  if (acpica_initialized_) {
    AcpiTerminate();
  }
}

zx_status_t X86::DoInit() {
  // Create the ACPI manager.
  fbl::AllocChecker ac;
  acpi_manager_ = ktl::make_unique<acpi::Manager>(&ac, acpi_.get());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = publish_acpi_devices(acpi_manager_.get());
  if (status != ZX_OK) {
    LTRACEF("publish_acpi_devices() failed: %d\n", status);
    return status;
  }

  return ZX_OK;
}

void X86::DdkInit(ddk::InitTxn txn) {}

void X86::DdkRelease() { delete this; }

zx_status_t X86::Create(void* ctx, zx_device_t* parent, std::unique_ptr<X86>* out) {
  acpi_rsdp = gPhysHandoff->acpi_rsdp.value_or(0);

  fbl::AllocChecker ac;
  auto acpi = ktl::make_unique<acpi::AcpiImpl>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = ktl::make_unique<X86>(&ac, parent, std::move(acpi));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

zx_status_t X86::CreateAndBind(void* ctx, zx_device_t* parent) {
  ktl::unique_ptr<X86> board;

  zx_status_t status = Create(ctx, parent, &board);
  if (status != ZX_OK) {
    return status;
  }

  status = board->Bind();
  if (status == ZX_OK) {
    // DevMgr now owns this pointer, release it to avoid destroying the
    // object when device goes out of scope.
    [[maybe_unused]] auto* ptr = board.release();
  }
  return status;
}

zx_status_t X86::Bind() {
  // Do early init of ACPICA etc.
  zx_status_t status = EarlyInit();
  if (status != ZX_OK) {
    LTRACEF("failed to perform early initialization %d \n", status);
    return status;
  }

  status = DoInit();
  if (status != ZX_OK) {
    LTRACEF("failed to initialize acpi manager: %d\n", status);
    return status;
  }

  return ZX_OK;
}

static zx_driver_ops_t x86_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = X86::CreateAndBind;
  // ops.run_unit_tests = X86::RunUnitTests;
  return ops;
}();

}  // namespace x86

MISTOS_DRIVER(acpi_bus, x86::x86_driver_ops, "mistos", "0.1", 0)
MISTOS_DRIVER_END(acpi_bus)
