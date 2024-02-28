// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/fidl.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>

#include <memory>

#include <ddktl/device.h>

#include "sdk/lib/driver/outgoing/cpp/outgoing_directory.h"

#define DRIVER_NAME "test-spi"

namespace spi {

class TestSpiDevice;
using DeviceType = ddk::Device<TestSpiDevice>;

class TestSpiDevice : public DeviceType, public fdf::WireServer<fuchsia_hardware_spiimpl::SpiImpl> {
 public:
  static zx_status_t Create(zx_device_t* parent) {
    auto dev = std::make_unique<TestSpiDevice>(parent, 0);
    pdev_protocol_t pdev;
    zx_status_t status;

    zxlogf(INFO, "TestSpiDevice::Create: %s ", DRIVER_NAME);

    status = device_get_protocol(parent, ZX_PROTOCOL_PDEV, &pdev);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: could not get ZX_PROTOCOL_PDEV", __func__);
      return status;
    }

    {
      fuchsia_hardware_spiimpl::Service::InstanceHandler handler({
          .device = dev->bindings_.CreateHandler(dev.get(), fdf::Dispatcher::GetCurrent()->get(),
                                                 fidl::kIgnoreBindingClosure),
      });
      auto result =
          dev->outgoing_.AddService<fuchsia_hardware_spiimpl::Service>(std::move(handler));
      if (result.is_error()) {
        zxlogf(ERROR, "AddService failed: %s", result.status_string());
        return result.error_value();
      }
    }

    auto directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (directory_endpoints.is_error()) {
      return directory_endpoints.status_value();
    }

    {
      auto result = dev->outgoing_.Serve(std::move(directory_endpoints->server));
      if (result.is_error()) {
        zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
        return result.error_value();
      }
    }

    std::array<const char*, 1> service_offers{fuchsia_hardware_spiimpl::Service::Name};
    status = dev->DdkAdd(ddk::DeviceAddArgs("test-spi")
                             .forward_metadata(parent, DEVICE_METADATA_SPI_CHANNELS)
                             .set_runtime_service_offers(service_offers)
                             .set_outgoing_dir(directory_endpoints->client.TakeChannel()));
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: DdkAdd failed: %d", __func__, status);
      return status;
    }

    status = dev->DdkAddMetadata(DEVICE_METADATA_PRIVATE, &dev->bus_id_, sizeof dev->bus_id_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: DdkAddMetadata failed: %d", __func__, status);
      return status;
    }

    // devmgr is now in charge of dev.
    [[maybe_unused]] auto ptr = dev.release();

    zxlogf(INFO, "%s: returning ZX_OK", __func__);
    return ZX_OK;
  }

  explicit TestSpiDevice(zx_device_t* parent, uint32_t bus_id)
      : DeviceType(parent), bus_id_(bus_id) {}

  void GetChipSelectCount(fdf::Arena& arena, GetChipSelectCountCompleter::Sync& completer) {
    completer.buffer(arena).Reply(1);
  }

  void TransmitVector(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVectorRequest* request,
                      fdf::Arena& arena, TransmitVectorCompleter::Sync& completer) {
    // TX only, ignore
    completer.buffer(arena).ReplySuccess();
  }

  void ReceiveVector(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVectorRequest* request,
                     fdf::Arena& arena, ReceiveVectorCompleter::Sync& completer) {
    fidl::VectorView<uint8_t> rxdata(arena, request->size);
    // RX only, fill with pattern
    for (size_t i = 0; i < rxdata.count(); i++) {
      rxdata[i] = i & 0xff;
    }
    completer.buffer(arena).ReplySuccess(rxdata);
  }

  void ExchangeVector(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVectorRequest* request,
                      fdf::Arena& arena, ExchangeVectorCompleter::Sync& completer) {
    fidl::VectorView<uint8_t> rxdata(arena, request->txdata.count());
    // Both TX and RX; copy
    memcpy(rxdata.data(), request->txdata.data(), request->txdata.count());
    completer.buffer(arena).ReplySuccess(rxdata);
  }

  void LockBus(fuchsia_hardware_spiimpl::wire::SpiImplLockBusRequest* request, fdf::Arena& arena,
               LockBusCompleter::Sync& completer) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void UnlockBus(fuchsia_hardware_spiimpl::wire::SpiImplUnlockBusRequest* request,
                 fdf::Arena& arena, UnlockBusCompleter::Sync& completer) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void RegisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplRegisterVmoRequest* request,
                   fdf::Arena& arena, RegisterVmoCompleter::Sync& completer) {}

  void UnregisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplUnregisterVmoRequest* request,
                     fdf::Arena& arena, UnregisterVmoCompleter::Sync& completer) {}

  void ReleaseRegisteredVmos(
      fuchsia_hardware_spiimpl::wire::SpiImplReleaseRegisteredVmosRequest* request,
      fdf::Arena& arena, ReleaseRegisteredVmosCompleter::Sync& completer) {}

  void TransmitVmo(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVmoRequest* request,
                   fdf::Arena& arena, TransmitVmoCompleter::Sync& completer) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void ReceiveVmo(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVmoRequest* request,
                  fdf::Arena& arena, ReceiveVmoCompleter::Sync& completer) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void ExchangeVmo(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVmoRequest* request,
                   fdf::Arena& arena, ExchangeVmoCompleter::Sync& completer) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  // Methods required by the ddk mixins
  void DdkRelease() { delete this; }

 private:
  uint32_t bus_id_;
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_spiimpl::SpiImpl> bindings_;
};

zx_status_t test_spi_bind(void* ctx, zx_device_t* parent) { return TestSpiDevice::Create(parent); }

constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = test_spi_bind;
  return driver_ops;
}();

}  // namespace spi

ZIRCON_DRIVER(test_spi, spi::driver_ops, "zircon", "0.1");
