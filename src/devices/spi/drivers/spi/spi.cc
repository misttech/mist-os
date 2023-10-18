// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi.h"

#include <fidl/fuchsia.hardware.spi.businfo/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

#include <fbl/alloc_checker.h>

#include "spi-child.h"
#include "src/devices/spi/drivers/spi/spi-impl-client.h"

namespace spi {

void SpiDevice::DdkRelease() { delete this; }

zx_status_t SpiDevice::Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher) {
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

  status = device->Init();
  if (status != ZX_OK) {
    return status;
  }

  status = device->DdkAdd(ddk::DeviceAddArgs("spi").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

  device->AddChildren(dispatcher);

  [[maybe_unused]] auto* dummy = device.release();

  return ZX_OK;
}

zx_status_t SpiDevice::Init() {
  /// NOTE: This driver is able to bind against both of the following protocols:
  ///
  ///  Fidl: //sdk/fidl/fuchsia.hardware.spiimpl
  /// Banjo: //sdk/banjo/fuchsia.hardware.spiimpl
  ///
  /// The driver will attempt to connect to both parent protocols and use
  /// whichever protocol is available from its parent.

  // Right now there isn't a way to detect if our FIDL connection was
  // successful.
  // We attempt to get the Banjo protocol from the parent and if that succeeds
  // we assume that we're operating in Banjo mode. If it fails we default to
  // FIDL.
  ddk::SpiImplProtocolClient spi(parent());
  if (spi.is_valid()) {
    spi_impl_ = BanjoSpiImplClient(spi);
    return ZX_OK;
  }

  auto client_end = DdkConnectFidlProtocol<fuchsia_hardware_spiimpl::Service::Device>();
  if (client_end.is_error()) {
    return client_end.error_value();
  }

  spi_impl_ = FidlSpiImplClient(std::move(client_end.value()));
  return ZX_OK;
}

void SpiDevice::AddChildren(async_dispatcher_t* dispatcher) {
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
    std::unique_ptr<SpiChild> dev(
        new (&ac) SpiChild(zxdev(), GetSpiImpl(), cs, has_siblings, dispatcher));
    if (!ac.check()) {
      zxlogf(ERROR, "Out of memory");
      return;
    }

    char name[20];
    snprintf(name, sizeof(name), "spi-%u-%u", bus_id, cs);

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      zxlogf(ERROR, "could not create fuchsia.io endpoints: %s", endpoints.status_string());
      return;
    }

    zx_status_t status = dev->ServeOutgoingDirectory(std::move(endpoints->server));
    if (status != ZX_OK) {
      zxlogf(ERROR, "could not serve outgoing directory: %s", zx_status_get_string(status));
      return;
    }

    std::array offers = {
        fuchsia_hardware_spi::Service::Name,
    };

    ddk::DeviceAddArgs args = ddk::DeviceAddArgs(name);

    std::vector<zx_device_prop_t> props;
    if (vid || pid || did) {
      props = std::vector<zx_device_prop_t>{
          {BIND_SPI_BUS_ID, 0, bus_id},    {BIND_SPI_CHIP_SELECT, 0, cs},
          {BIND_PLATFORM_DEV_VID, 0, vid}, {BIND_PLATFORM_DEV_PID, 0, pid},
          {BIND_PLATFORM_DEV_DID, 0, did},
      };
    } else {
      props = std::vector<zx_device_prop_t>{
          {BIND_SPI_BUS_ID, 0, bus_id},
          {BIND_SPI_CHIP_SELECT, 0, cs},
      };
    }
    args.set_props(props);

    status = dev->DdkAdd(args.set_fidl_service_offers(offers)
                             .set_proto_id(ZX_PROTOCOL_SPI)
                             .set_outgoing_dir(endpoints->client.TakeChannel()));
    if (status != ZX_OK) {
      zxlogf(ERROR, "DdkAdd failed for SPI child device: %s", zx_status_get_string(status));
      return;
    }

    // Owned by the framework now.
    [[maybe_unused]] auto ptr = dev.release();
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
