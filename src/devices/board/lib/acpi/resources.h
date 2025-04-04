// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_LIB_ACPI_RESOURCES_H_
#define SRC_DEVICES_BOARD_LIB_ACPI_RESOURCES_H_

#ifndef __mist_os__
#include <fidl/fuchsia.hardware.i2c.businfo/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.spi.businfo/cpp/natural_types.h>
#endif  // __mist_os__

#include <zircon/types.h>

#include <acpica/acpi.h>

#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/status.h"

struct DeviceResources {
#ifndef __mist_os__
  std::vector<fuchsia_hardware_spi_businfo::SpiChannel> spi;
  std::vector<fuchsia_hardware_i2c_businfo::I2CChannel> i2c;
#endif  // __mist_os__
};

enum resource_address_type {
  RESOURCE_ADDRESS_MEMORY,
  RESOURCE_ADDRESS_IO,
  RESOURCE_ADDRESS_BUS_NUMBER,
  RESOURCE_ADDRESS_UNKNOWN,
};

// Structure that unifies the 3 Memory resource types
typedef struct resource_memory {
  bool writeable;
  uint32_t minimum;  // min base address
  uint32_t maximum;  // max base address
  uint32_t alignment;
  uint32_t address_length;
} resource_memory_t;

// Structure that unifies the 4 Address resource types
typedef struct resource_address {
  // Interpretation of min/max depend on the min/max_address_fixed flags
  // below.
  uint64_t minimum;
  uint64_t maximum;
  uint64_t address_length;

  uint64_t translation_offset;
  uint64_t granularity;

  enum resource_address_type resource_type;
  bool consumed_only;
  bool subtractive_decode;
  bool min_address_fixed;
  bool max_address_fixed;
} resource_address_t;

typedef struct resource_io {
  bool decodes_full_space;  // If false, only decodes 10-bits
  uint8_t alignment;
  uint8_t address_length;
  uint16_t minimum;
  uint16_t maximum;
} resource_io_t;

typedef struct resource_irq {
  uint8_t trigger;
  uint8_t polarity;
  uint8_t sharable;
  uint8_t wake_capable;
  uint8_t pin_count;
  uint32_t pins[16];
} resource_irq_t;

bool resource_is_memory(ACPI_RESOURCE* res);
bool resource_is_address(ACPI_RESOURCE* res);
bool resource_is_io(ACPI_RESOURCE* res);
bool resource_is_irq(ACPI_RESOURCE* res);
bool resource_is_spi(ACPI_RESOURCE* res);
bool resource_is_i2c(ACPI_RESOURCE* res);

zx_status_t resource_parse_memory(ACPI_RESOURCE* res, resource_memory_t* out);
zx_status_t resource_parse_address(ACPI_RESOURCE* res, resource_address_t* out);
zx_status_t resource_parse_io(ACPI_RESOURCE* res, resource_io_t* out);
zx_status_t resource_parse_irq(ACPI_RESOURCE* res, resource_irq_t* out);

// Parse the given SPI resource.
// Arguments:
// |acpi| - ACPI implementation.
// |device| - Device to which this resource belongs.
// |res| - Resource to parse.
// |resource_source| - Pointer which will have the ResourceSource's handle put into it.
acpi::status<> resource_parse_spi(acpi::Acpi* acpi, ACPI_HANDLE device, ACPI_RESOURCE* res,
                                  ACPI_HANDLE* resource_source);

// Parse the given I2C resource.
// Arguments:
// |acpi| - ACPI implementation.
// |device| - Device to which this resource belongs.
// |res| - Resource to parse.
// |resource_source| - Pointer which will have the ResourceSource's handle put into it.
acpi::status<> resource_parse_i2c(acpi::Acpi* acpi, ACPI_HANDLE device, ACPI_RESOURCE* res,
                                  ACPI_HANDLE* resource_source);

#endif  // SRC_DEVICES_BOARD_LIB_ACPI_RESOURCES_H_
