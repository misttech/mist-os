// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_ACPICA_OSZIRCON_H_
#define ZIRCON_SYSTEM_ULIB_ACPICA_OSZIRCON_H_

#include <stdio.h>
#include <trace.h>

#include <acpica/acpi.h>

#define _COMPONENT ACPI_OS_SERVICES
ACPI_MODULE_NAME("oszircon")

#define LOCAL_TRACE 0

// Start the task execution thread.
ACPI_STATUS AcpiTaskThreadStart();
// Terminate the task execution thread.
ACPI_STATUS AcpiTaskThreadTerminate();

// Set up IO ports.
ACPI_STATUS AcpiIoPortSetup();

#endif  // ZIRCON_SYSTEM_ULIB_ACPICA_OSZIRCON_H_
