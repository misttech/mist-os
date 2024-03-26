// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi.h"

#include <fidl/fuchsia.hardware.spi.businfo/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/function.h>

#include <fbl/alloc_checker.h>

#include "spi-child.h"

namespace spi {

void SpiDevice::DdkRelease() { delete this; }

zx_status_t SpiDevice::Create(void* ctx, zx_device_t* parent) {
  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_spi_businfo::wire::SpiBusMetadata>(
      parent, DEVICE_METADATA_SPI_CHANNELS);
  if (!decoded.is_ok()) {
    zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    return decoded.error_value();
  }

  fuchsia_hardware_spi_businfo::wire::SpiBusMetadata& metadata = *decoded.value();
  if (!metadata.has_bus_id()) {
    zxlogf(ERROR, "No bus ID metadata provided");
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<SpiDevice> device(new (&ac) SpiDevice(parent, metadata.bus_id()));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = device->Init();
  if (status != ZX_OK) {
    return status;
  }

  status = device->DdkAdd(ddk::DeviceAddArgs("spi").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

  if (!metadata.has_channels()) {
    zxlogf(INFO, "No channels supplied.");
  } else {
    zxlogf(INFO, "%zu channels supplied.", metadata.channels().count());

    async::PostTask(device->fidl_dispatcher()->async_dispatcher(),
                    [device = device.get(), metadata = *std::move(decoded)]() mutable {
                      device->AddChildren(*metadata, device->spi_impl_.Clone());
                    });
  }

  [[maybe_unused]] auto* dummy = device.release();

  return ZX_OK;
}

zx_status_t SpiDevice::Init() {
  auto scheduler_role = ddk::GetEncodedMetadata<fuchsia_scheduler::wire::RoleName>(
      parent(), DEVICE_METADATA_SCHEDULER_ROLE_NAME);
  if (scheduler_role.is_ok()) {
    const std::string role_name(scheduler_role->role.get());

    zx::result result = fdf::SynchronizedDispatcher::Create(
        {}, "SPI", fit::bind_member<&SpiDevice::DispatcherShutdownHandler>(this), role_name);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to create SynchronizedDispatcher: %s", result.status_string());
      return result.error_value();
    }

    // If scheduler role metadata was provided, create a new dispatcher using the role, and use that
    // dispatcher instead of the default dispatcher passed to this method.
    fidl_dispatcher_.emplace(*std::move(result));

    zxlogf(DEBUG, "Using dispatcher with role \"%s\"", role_name.c_str());
  }

  auto client_end = DdkConnectRuntimeProtocol<fuchsia_hardware_spiimpl::Service::Device>();
  if (client_end.is_error()) {
    return client_end.error_value();
  }

  spi_impl_.Bind(
      *std::move(client_end), fidl_dispatcher()->get(),
      fidl::ObserveTeardown(fit::bind_member<&SpiDevice::FidlClientTeardownHandler>(this)));
  return ZX_OK;
}

void SpiDevice::AddChildren(const fuchsia_hardware_spi_businfo::wire::SpiBusMetadata& metadata,
                            fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> client) {
  bool has_siblings = metadata.channels().count() > 1;
  for (auto& channel : metadata.channels()) {
    const auto cs = channel.has_cs() ? channel.cs() : 0;
    const auto vid = channel.has_vid() ? channel.vid() : 0;
    const auto pid = channel.has_pid() ? channel.pid() : 0;
    const auto did = channel.has_did() ? channel.did() : 0;

    fbl::AllocChecker ac;

    std::unique_ptr<SpiChild> dev(
        new (&ac) SpiChild(zxdev(), client.Clone(), cs, has_siblings, fidl_dispatcher()));

    if (!ac.check()) {
      zxlogf(ERROR, "Out of memory");
      return;
    }

    char name[20];
    snprintf(name, sizeof(name), "spi-%u-%u", bus_id_, cs);

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
          {BIND_SPI_BUS_ID, 0, bus_id_},   {BIND_SPI_CHIP_SELECT, 0, cs},
          {BIND_PLATFORM_DEV_VID, 0, vid}, {BIND_PLATFORM_DEV_PID, 0, pid},
          {BIND_PLATFORM_DEV_DID, 0, did},
      };
    } else {
      props = std::vector<zx_device_prop_t>{
          {BIND_SPI_BUS_ID, 0, bus_id_},
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

void SpiDevice::DdkUnbind(ddk::UnbindTxn txn) {
  ZX_DEBUG_ASSERT_MSG(!unbind_txn_, "Unbind txn already set");

  if (!fidl_dispatcher_) {
    dispatcher_shutdown_complete_ = true;
  }

  // Reply immediately if we don't have a FIDL client or dispatcher, or if they have already been
  // torn down.
  if (fidl_client_teardown_complete_ && dispatcher_shutdown_complete_) {
    return txn.Reply();
  }

  unbind_txn_ = std::move(txn);
  spi_impl_.AsyncTeardown();
  if (fidl_dispatcher_) {
    // Post a task to the FIDL dispatcher to start shutting itself down. If run on this thread
    // instead, the dispatcher could change states while a task on it is running, potentially
    // causing an assert failure if that task was trying to bind a FIDL server.
    async::PostTask(fidl_dispatcher()->async_dispatcher(),
                    [this]() { fidl_dispatcher_->ShutdownAsync(); });
  }
}

void SpiDevice::FidlClientTeardownHandler() {
  async::PostTask(driver_dispatcher_->async_dispatcher(), [this]() {
    fidl_client_teardown_complete_ = true;
    if (unbind_txn_ && dispatcher_shutdown_complete_) {
      unbind_txn_->Reply();
    }
  });
}

void SpiDevice::DispatcherShutdownHandler(fdf_dispatcher_t* dispatcher) {
  async::PostTask(driver_dispatcher_->async_dispatcher(), [this]() {
    dispatcher_shutdown_complete_ = true;
    if (unbind_txn_ && fidl_client_teardown_complete_) {
      unbind_txn_->Reply();
    }
  });
}

static zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = SpiDevice::Create;
  return ops;
}();

}  // namespace spi

ZIRCON_DRIVER(spi, spi::driver_ops, "zircon", "0.1");
