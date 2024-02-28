// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spi/cpp/fidl.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <fuchsia/hardware/test/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <ddktl/device.h>

#include "src/devices/bus/drivers/platform/test/test-metadata.h"

#define DRIVER_NAME "test-composite"
#define GOLDFISH_TEST_HEAP (0x100000000000fffful)

typedef struct {
  zx_device_t* zxdev;
} test_t;

typedef struct {
  uint32_t magic;
} mode_config_magic_t;

typedef struct {
  uint32_t mode;
  union {
    mode_config_magic_t magic;
  };
} mode_config_t;

class TestCompositeDevice;
using DeviceType = ddk::Device<TestCompositeDevice>;

class TestCompositeDevice : public DeviceType {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  explicit TestCompositeDevice(zx_device_t* parent) : DeviceType(parent) {}

  void DdkRelease() { delete this; }
};

static zx_status_t test_gpio(fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> gpio_client) {
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio(std::move(gpio_client));

  if (auto result = gpio->ConfigOut(0); !result.ok()) {
    return result.status();
  } else if (result->is_error()) {
    return result->error_value();
  }

  if (auto result = gpio->Read(); !result.ok()) {
    return result.status();
  } else if (result->is_error()) {
    return result->error_value();
  } else if (result->value()->value != 0) {
    return ZX_ERR_BAD_STATE;
  }

  if (auto result = gpio->Write(1); !result.ok()) {
    return result.status();
  } else if (result->is_error()) {
    return result->error_value();
  }

  if (auto result = gpio->Read(); !result.ok()) {
    return result.status();
  } else if (result->is_error()) {
    return result->error_value();
  } else if (result->value()->value != 1) {
    return ZX_ERR_BAD_STATE;
  }

  return ZX_OK;
}

static zx_status_t test_spi(fidl::ClientEnd<fuchsia_hardware_spi::Device> spi_client) {
  fidl::WireSyncClient<fuchsia_hardware_spi::Device> spi(std::move(spi_client));

  uint8_t txbuf[] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
  auto txbuf_view = fidl::VectorView<uint8_t>::FromExternal(txbuf, sizeof txbuf);

  // tx should just succeed
  if (auto result = spi->TransmitVector(txbuf_view); !result.ok()) {
    zxlogf(ERROR, "Call to TransmitVector failed: %s", result.FormatDescription().c_str());
    return result.status();
  } else if (result->status != ZX_OK) {
    zxlogf(ERROR, "TransmitVector failed: %s", zx_status_get_string(result->status));
    return result->status;
  }

  // rx should return pattern
  if (auto result = spi->ReceiveVector(sizeof txbuf); !result.ok()) {
    zxlogf(ERROR, "Call to ReceiveVector failed: %s", result.FormatDescription().c_str());
    return result.status();
  } else if (result->status != ZX_OK) {
    zxlogf(ERROR, "ReceiveVector failed: %s", zx_status_get_string(result->status));
    return result->status;
  } else if (result->data.count() != sizeof txbuf) {
    zxlogf(ERROR, "ReceiveVector returned incomplete %zu/%zu (", result->data.count(),
           sizeof txbuf);
    return ZX_ERR_INTERNAL;
  } else {
    for (size_t i = 0; i < result->data.count(); i++) {
      if (result->data[i] != (i & 0xff)) {
        zxlogf(ERROR, "ReceiveVector returned bad pattern rxbuf[%zu] = 0x%02x, should be 0x%02x(",
               i, result->data[i], (uint8_t)(i & 0xff));
        return ZX_ERR_INTERNAL;
      }
    }
  }

  // exchange copies input
  if (auto result = spi->ExchangeVector(txbuf_view); !result.ok()) {
    zxlogf(ERROR, "Call to ExchangeVector failed: %s", result.FormatDescription().c_str());
    return result.status();
  } else if (result->status != ZX_OK) {
    zxlogf(ERROR, "ExchangeVector failed: %s", zx_status_get_string(result->status));
    return result->status;
  } else if (result->rxdata.count() != sizeof txbuf) {
    zxlogf(ERROR, "ExchangeVector returned incomplete %zu/%zu (", result->rxdata.count(),
           sizeof txbuf);
    return ZX_ERR_INTERNAL;
  } else {
    for (size_t i = 0; i < result->rxdata.count(); i++) {
      if (result->rxdata[i] != txbuf[i]) {
        zxlogf(ERROR, "ExchangeVector returned bad result rxbuf[%zu] = 0x%02x, should be 0x%02x(",
               i, result->rxdata[i], txbuf[i]);
        return ZX_ERR_INTERNAL;
      }
    }
  }

  return ZX_OK;
}

zx_status_t TestCompositeDevice::Bind(void* ctx, zx_device_t* parent) {
  zxlogf(INFO, "test_bind: %s ", DRIVER_NAME);

  pdev_protocol_t pdev;
  zx_status_t status = device_get_fragment_protocol(parent, "pdev", ZX_PROTOCOL_PDEV, &pdev);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not get protocol ZX_PROTOCOL_PDEV", DRIVER_NAME);
    return status;
  }

  // Make sure we can read metadata added to a fragment.
  size_t size;
  composite_test_metadata metadata;
  status = device_get_fragment_metadata(parent, "pdev", DEVICE_METADATA_PRIVATE, &metadata,
                                        sizeof(metadata), &size);
  if (status != ZX_OK || size != sizeof(composite_test_metadata)) {
    zxlogf(ERROR, "%s: device_get_metadata failed: %d", DRIVER_NAME, status);
    return ZX_ERR_INTERNAL;
  }

  if (metadata.metadata_value != 12345) {
    zxlogf(ERROR, "%s: device_get_metadata failed: %d", DRIVER_NAME, status);
    return ZX_ERR_INTERNAL;
  }

  if (metadata.composite_device_id == PDEV_DID_TEST_COMPOSITE_1) {
    auto result =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(parent, "gpio");
    if (result.is_error()) {
      zxlogf(ERROR, "%s: failed to connect to GPIO: %s", DRIVER_NAME, result.status_string());
      return result.status_value();
    }
    if ((status = test_gpio(*std::move(result))) != ZX_OK) {
      zxlogf(ERROR, "%s: test_gpio failed: %d", DRIVER_NAME, status);
      return status;
    }
  } else if (metadata.composite_device_id == PDEV_DID_TEST_COMPOSITE_2) {
    auto result =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_spi::Service::Device>(parent, "spi");
    if (result.is_error()) {
      zxlogf(ERROR, "%s: failed to connect to SPI: %s", DRIVER_NAME, result.status_string());
      return result.status_value();
    }
    if ((status = test_spi(*std::move(result))) != ZX_OK) {
      zxlogf(ERROR, "%s: test_spi failed: %d", DRIVER_NAME, status);
      return status;
    }
  }

  auto test = std::make_unique<TestCompositeDevice>(parent);

  status = test->DdkAdd(ddk::DeviceAddArgs("composite").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: device_add failed: %d", DRIVER_NAME, status);
    return status;
  }

  [[maybe_unused]] auto unused = test.release();

  return ZX_OK;
}

static zx_driver_ops_t test_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = TestCompositeDevice::Bind,
};

ZIRCON_DRIVER(test_composite, test_driver_ops, "zircon", "0.1");
