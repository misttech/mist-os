// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/init.h"

#include <lib/ddk/binding_priv.h>
#include <lib/ddk/driver.h>

#include <misc/drivers/mistos/driver.h>

// A linear array of statically defined drivers.
extern zircon_driver_ldr_t __start_mistos_driver_ldr[];
extern zircon_driver_ldr_t __stop_mistos_driver_ldr[];

void mistos_driver_init() {
  for (const zircon_driver_ldr_t* note = __start_mistos_driver_ldr;
       note != __stop_mistos_driver_ldr; ++note) {

    auto driver = mistos::CreateDriver(note);
    if (driver == nullptr) {
      dprintf(CRITIAL, "Failed to create driver '%s'\n", note->note.payload.name);
    }
  }
}
