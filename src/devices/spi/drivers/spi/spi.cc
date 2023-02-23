// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi.h"

#include <fidl/fuchsia.hardware.spi.businfo/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

#include <fbl/alloc_checker.h>

#include "spi-child.h"
#include "src/devices/spi/drivers/spi/spi_bind.h"

namespace spi {

void SpiDevice::DdkRelease() { delete this; }

zx_status_t SpiDevice::Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher) {
  ddk::SpiImplProtocolClient spi(parent);
  if (!spi.is_valid()) {
    return ZX_ERR_NO_RESOURCES;
  }

  uint32_t bus_id;
  size_t actual;
  zx_status_t status =
      device_get_metadata(parent, DEVICE_METADATA_PRIVATE, &bus_id, sizeof bus_id, &actual);
  if (status != ZX_OK) {
    return status;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<SpiDevice> device(new (&ac) SpiDevice(parent, bus_id));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  status = device->DdkAdd(ddk::DeviceAddArgs("spi").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

  device->AddChildren(spi, dispatcher);

  [[maybe_unused]] auto* dummy = device.release();

  return ZX_OK;
}

void SpiDevice::AddChildren(const ddk::SpiImplProtocolClient& spi, async_dispatcher_t* dispatcher) {
  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_spi_businfo::wire::SpiBusMetadata>(
      parent(), DEVICE_METADATA_SPI_CHANNELS);
  if (!decoded.is_ok()) {
    return;
  }

  fuchsia_hardware_spi_businfo::wire::SpiBusMetadata& metadata = *decoded.value();
  if (!metadata.has_channels()) {
    zxlogf(INFO, "No channels supplied.");
    return;
  }
  zxlogf(INFO, "%zu channels supplied.", metadata.channels().count());

  bool has_siblings = metadata.channels().count() > 1;
  for (auto& channel : metadata.channels()) {
    const auto bus_id = channel.has_bus_id() ? channel.bus_id() : 0;

    if (bus_id != bus_id_) {
      continue;
    }

    const auto cs = channel.has_cs() ? channel.cs() : 0;
    const auto vid = channel.has_vid() ? channel.vid() : 0;
    const auto pid = channel.has_pid() ? channel.pid() : 0;
    const auto did = channel.has_did() ? channel.did() : 0;

    fbl::AllocChecker ac;
    std::unique_ptr<SpiChild> dev(new (&ac) SpiChild(zxdev(), spi, cs, has_siblings, dispatcher));
    if (!ac.check()) {
      zxlogf(ERROR, "Out of memory");
      return;
    }

    char name[20];
    snprintf(name, sizeof(name), "spi-%u-%u", bus_id, cs);

    char fidl_name[25];
    snprintf(fidl_name, sizeof(fidl_name), "spi-fidl-%u-%u", bus_id, cs);

    char banjo_name[26];
    snprintf(banjo_name, sizeof(banjo_name), "spi-banjo-%u-%u", bus_id, cs);

    // The SpiChild device is non-bindable and exists to serve as the parent of the FIDL and Banjo
    // children, which are bindable.
    //
    // Setting the proto ID to ZX_PROTOCOL_SPI creates an entry for this device in /dev/class/spi.
    zx_status_t status = dev->DdkAdd(
        ddk::DeviceAddArgs(name).set_flags(DEVICE_ADD_NON_BINDABLE).set_proto_id(ZX_PROTOCOL_SPI));
    if (status != ZX_OK) {
      zxlogf(ERROR, "DdkAdd failed for SPI child device: %s", zx_status_get_string(status));
      return;
    }

    // Owned by the framework now.
    [[maybe_unused]] auto ptr = dev.release();

    // Create the FIDL child.
    auto fidl_dev = std::make_unique<SpiFidlChild>(ptr->zxdev(), ptr, dispatcher);

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      zxlogf(ERROR, "could not create fuchsia.io endpoints: %s", endpoints.status_string());
      return;
    }

    status = fidl_dev->ServeOutgoingDirectory(std::move(endpoints->server));
    if (status != ZX_OK) {
      zxlogf(ERROR, "could not serve outgoing directory: %s", zx_status_get_string(status));
      return;
    }

    std::array offers = {
        fidl::DiscoverableProtocolName<fuchsia_hardware_spi::Device>,
    };

    status = fidl_dev->DdkAdd(ddk::DeviceAddArgs(fidl_name)
                                  .set_flags(DEVICE_ADD_MUST_ISOLATE)
                                  .set_fidl_protocol_offers(offers)
                                  .set_outgoing_dir(endpoints->client.TakeChannel()));

    if (status != ZX_OK) {
      zxlogf(ERROR, "DdkAdd failed for FIDL device: %s", zx_status_get_string(status));
      return;
    }

    // Owned by the framework now.
    [[maybe_unused]] auto fidl_ptr = fidl_dev.release();

    // Create the Banjo child.
    auto banjo_dev = std::make_unique<SpiBanjoChild>(ptr->zxdev(), ptr);
    if (vid || pid || did) {
      // BIND_PROTOCOL is manually specified in the bind properties instead of using the proto_id
      // arg to DdkAdd to avoid creating a /dev/class/spi entry for this node; instead, the
      // /dev/class/spi entry is backed by the SpiChild class.
      zx_device_prop_t props[] = {
          {BIND_PROTOCOL, 0, ZX_PROTOCOL_SPI}, {BIND_SPI_BUS_ID, 0, bus_id},
          {BIND_SPI_CHIP_SELECT, 0, cs},       {BIND_PLATFORM_DEV_VID, 0, vid},
          {BIND_PLATFORM_DEV_PID, 0, pid},     {BIND_PLATFORM_DEV_DID, 0, did},
      };

      status = banjo_dev->DdkAdd(ddk::DeviceAddArgs(banjo_name).set_props(props));
    } else {
      zx_device_prop_t props[] = {
          {BIND_PROTOCOL, 0, ZX_PROTOCOL_SPI},
          {BIND_SPI_BUS_ID, 0, bus_id},
          {BIND_SPI_CHIP_SELECT, 0, cs},
      };

      status = banjo_dev->DdkAdd(ddk::DeviceAddArgs(banjo_name).set_props(props));
    }

    if (status != ZX_OK) {
      zxlogf(ERROR, "DdkAdd failed for Banjo device: %s", zx_status_get_string(status));
      return;
    }

    // Owned by the framework now.
    [[maybe_unused]] auto banjo_ptr = banjo_dev.release();
  }
}

static zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = [](void* ctx, zx_device_t* parent) {
    return SpiDevice::Create(
        ctx, parent, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()));
  };
  return ops;
}();

}  // namespace spi

ZIRCON_DRIVER(spi, spi::driver_ops, "zircon", "0.1");
