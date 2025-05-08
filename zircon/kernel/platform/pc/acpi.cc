// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "platform/pc/acpi.h"

#include <align.h>
#include <lib/acpi_lite.h>
#include <lib/acpi_lite/structures.h>
#include <lib/acpi_lite/zircon.h>
#include <lib/console.h>
#include <lib/fit/defer.h>
#include <lib/lazy_init/lazy_init.h>
#include <lib/zx/result.h>
#include <trace.h>
#include <zircon/types.h>

#include <arch/x86/acpi.h>
#include <arch/x86/bootstrap16.h>
#include <kernel/percpu.h>
#include <ktl/optional.h>
#include <lk/init.h>
#include <phys/handoff.h>

#include <ktl/enforce.h>

namespace {

// System-wide ACPI parser.
acpi_lite::AcpiParser* global_acpi_parser;

lazy_init::LazyInit<acpi_lite::AcpiParser, lazy_init::CheckType::None,
                    lazy_init::Destructor::Disabled>
    g_parser;

int ConsoleAcpiDump(int argc, const cmd_args* argv, uint32_t flags) {
  if (global_acpi_parser == nullptr) {
    printf("ACPI not initialized.\n");
    return 1;
  }

  global_acpi_parser->DumpTables();
  return 0;
}

}  // namespace

acpi_lite::AcpiParser& GlobalAcpiLiteParser() {
  ASSERT_MSG(global_acpi_parser != nullptr, "PlatformInitAcpi() not called.");
  return *global_acpi_parser;
}

void PlatformInitAcpi(zx_paddr_t acpi_rsdp) {
  ASSERT(global_acpi_parser == nullptr);

  // Create AcpiParser.
  zx::result<acpi_lite::AcpiParser> result = acpi_lite::AcpiParserInit(acpi_rsdp);
  if (result.is_error()) {
    panic("Could not initialize ACPI. Error code: %d.", result.error_value());
  }

  g_parser.Initialize(result.value());
  global_acpi_parser = &g_parser.Get();
}

static void platform_init_acpi(uint level) {
  PlatformInitAcpi(gPhysHandoff->acpi_rsdp.value_or(0));
}

LK_INIT_HOOK(platform_init_acpi, platform_init_acpi, LK_INIT_LEVEL_VM)

STATIC_COMMAND_START
STATIC_COMMAND("acpidump", "dump ACPI tables to console", &ConsoleAcpiDump)
STATIC_COMMAND_END(acpi)
