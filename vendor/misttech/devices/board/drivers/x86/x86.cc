// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vendor/misttech/devices/board/drivers/x86/x86.h"

#include <trace.h>

#include <acpica/acpi.h>
#include <lk/init.h>
#include <phys/handoff.h>

#include "vendor/misttech/devices/board/drivers/x86/include/acpi.h"

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

zx_status_t X86::Create(void* ctx, ktl::unique_ptr<X86>* out) {
  acpi_rsdp = gPhysHandoff->acpi_rsdp.value_or(0);

  fbl::AllocChecker ac;
  auto acpi = ktl::make_unique<acpi::AcpiImpl>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = ktl::make_unique<X86>(&ac, std::move(acpi));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

zx_status_t X86::CreateAndBind(void* ctx) {
  ktl::unique_ptr<X86> board;

  zx_status_t status = Create(ctx, &board);
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

}  // namespace x86

void acpi_bus(uint level) { ZX_ASSERT(x86::X86::CreateAndBind(nullptr) == ZX_OK); }

LK_INIT_HOOK(acpi_bus, acpi_bus, LK_INIT_LEVEL_PLATFORM)
