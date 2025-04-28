// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/devmgr/init.h"

#include <lib/console.h>
#include <lib/lazy_init/lazy_init.h>
#include <lib/mistos/devmgr/coordinator.h>
#include <lib/mistos/devmgr/driver.h>
#include <trace.h>

#include <ktl/unique_ptr.h>

#define LOCAL_TRACE 0

// A linear array of statically defined drivers.
extern const zircon_driver_note_t __start_mistos_driver_ldr[];
extern const zircon_driver_note_t __stop_mistos_driver_ldr[];

#ifndef ROOT_DRIVER_NAME
#define ROOT_DRIVER_NAME platform_bus
#endif

/* macro-expanding concat */
#define concat(a, b) __ex_concat(a, b)
#define __ex_concat(a, b) a##b

// Define the prefix separately for clarity
#define DRIVER_LDR_PREFIX __mistos_driver_ldr_
#define ROOT_DRIVER_LD concat(DRIVER_LDR_PREFIX, ROOT_DRIVER_NAME)

extern zircon_driver_note_t ROOT_DRIVER_LD;

namespace {

// System-wide coordinator.
devmgr::Coordinator* global_coordinator;

lazy_init::LazyInit<devmgr::Coordinator, lazy_init::CheckType::None,
                    lazy_init::Destructor::Disabled>
    g_coordinator;

int cmd_driver(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments\n");
  usage:
    printf("%s dump\n", argv[0].str);
    printf("%s list\n", argv[0].str);
    return -1;
  }

  if (!strcmp(argv[1].str, "dump")) {
    if (argc < 2) {
      goto notenoughargs;
    }
    g_coordinator->DumpState();
  } else if (!strcmp(argv[1].str, "list")) {
    g_coordinator->DumpDrivers();
  } else {
    printf("invalid args\n");
    goto usage;
  }
  return 0;
}

}  // namespace

devmgr::Coordinator& GlobalCoordinator() {
  ASSERT_MSG(global_coordinator != nullptr, "mistos_devmgr_init() not called.");
  return *global_coordinator;
}

void mistos_devmgr_init() {
  g_coordinator.Initialize();
  global_coordinator = &g_coordinator.Get();

  devmgr::find_loadable_drivers(
      __start_mistos_driver_ldr, __stop_mistos_driver_ldr,
      fit::bind_member<&devmgr::Coordinator::DriverAddedInit>(&g_coordinator.Get()));

  zircon_driver_note_t* root_driver = &ROOT_DRIVER_LD;
  dprintf(INFO, "Starting with root driver: [%s]\n", root_driver->payload.name);

  auto start_args = mistos::DriverStartArgs();
  auto result = g_coordinator->StartDriver(std::move(start_args), root_driver->payload.name);
  if (result.is_error()) {
    dprintf(CRITICAL, "Failed to start root driver: [%s], %d\n", root_driver->payload.name,
            result.error_value());
    return;
  }
}

STATIC_COMMAND_START
STATIC_COMMAND("driver", "driver info", &cmd_driver)
STATIC_COMMAND_END(driver)
