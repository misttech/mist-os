// Copyright (c) 2019 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sdio/sdio_device.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>
#include <lib/zircon-internal/align.h>

#include <limits>
#include <string>

#include <bind/fuchsia/wlan/phyimpl/cpp/bind.h>
#include <ddktl/device.h>
#include <ddktl/init-txn.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/bus.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/core.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/inspect/device_inspect.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sdio/sdio.h"

namespace wlan {
namespace brcmfmac {

SdioDevice::SdioDevice(zx_device_t* parent) : Device(parent) {}

SdioDevice::~SdioDevice() = default;

void SdioDevice::Shutdown() {
  if (async_loop_) {
    // Explicitly destroy the async loop before further shutdown to prevent asynchronous tasks
    // from using resources as they are being deallocated.
    async_loop_.reset();
  }
  if (brcmf_bus_) {
    brcmf_sdio_exit(brcmf_bus_.get());
    brcmf_bus_.reset();
  }
}

// static
zx_status_t SdioDevice::Create(zx_device_t* parent_device) {
  zx_status_t status = ZX_OK;

  auto async_loop = std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread);
  if ((status = async_loop->StartThread("brcmfmac-worker", nullptr)) != ZX_OK) {
    return status;
  }

  std::unique_ptr<DeviceInspect> inspect;
  if ((status = DeviceInspect::Create(async_loop->dispatcher(), &inspect)) != ZX_OK) {
    return status;
  }

  std::unique_ptr<SdioDevice> device(new SdioDevice(parent_device));
  device->async_loop_ = std::move(async_loop);
  device->inspect_ = std::move(inspect);

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  status = device->ServeWlanPhyImplProtocol(std::move(endpoints->server));
  if (status != ZX_OK) {
    return status;
  }

  std::array<const char*, 1> offers{
      fuchsia_wlan_phyimpl::Service::Name,
  };

  zx_device_str_prop_t props[] = {
      {
          .key = bind_fuchsia_wlan_phyimpl::WLANPHYIMPL.c_str(),
          .property_value =
              str_prop_str_val(bind_fuchsia_wlan_phyimpl::WLANPHYIMPL_DRIVERTRANSPORT.c_str()),
      },
  };

  if ((status = device->DdkAdd(
           ddk::DeviceAddArgs("brcmfmac-wlanphy")
               .set_str_props(props)
               .set_inspect_vmo(device->inspect_->inspector().DuplicateVmo())
               .set_runtime_service_offers(offers)
               .set_outgoing_dir(endpoints->client.TakeChannel())
               .forward_metadata(parent_device, DEVICE_METADATA_WIFI_CONFIG)
               .forward_metadata(parent_device, DEVICE_METADATA_MAC_ADDRESS))) != ZX_OK) {
    return status;
  }
  device.release();  // This now has its lifecycle managed by the devhost.

  // Further initialization is performed in the SdioDevice::Init() DDK hook, invoked by the devhost.
  return ZX_OK;
}

async_dispatcher_t* SdioDevice::GetDispatcher() { return async_loop_->dispatcher(); }

DeviceInspect* SdioDevice::GetInspect() { return inspect_.get(); }

zx_status_t SdioDevice::DeviceInit() {
  zx_status_t status = ZX_OK;

  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> fidl_gpios[GPIO_COUNT] = {};

  zx::result client_end = DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
      drvr()->device->parent(), "gpio-oob");
  if (client_end.is_error()) {
    BRCMF_ERR("Failed to connect to oob GPIO service: %s", client_end.status_string());
    return client_end.status_value();
  }
  fidl_gpios[WIFI_OOB_IRQ_GPIO_INDEX].Bind(std::move(client_end.value()));

  // Attempt to connect to the DEBUG GPIO, ignore if not available.
  client_end = DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
      drvr()->device->parent(), "gpio-debug");
  if (client_end.is_error()) {
    BRCMF_DBG(SDIO, "Failed to connect to debug GPIO service: %s", client_end.status_string());
  } else {
    fidl_gpios[DEBUG_GPIO_INDEX].Bind(std::move(client_end.value()));
  }

  std::unique_ptr<brcmf_bus> bus;
  if ((status = brcmf_sdio_register(drvr(), fidl_gpios, &bus)) != ZX_OK) {
    BRCMF_ERR("brcmf_sdio_register failed: %s", zx_status_get_string(status));
    return status;
  }

  // If the method fails after this point the bus object has to be destroyed by calling
  // brcmf_sdio_exit to ensure proper destruction of all objects created during registration.
  auto on_error = fit::defer([bus = bus.get()] { brcmf_sdio_exit(bus); });

  if ((status = brcmf_sdio_load_files(drvr(), false)) != ZX_OK) {
    BRCMF_ERR("brcmf_sdio_load_files failed: %s", zx_status_get_string(status));
    return status;
  }

  if ((status = brcmf_bus_started(drvr(), false)) != ZX_OK) {
    BRCMF_ERR("brcmf_bus_started failed: %s", zx_status_get_string(status));
    return status;
  }

  // Now that everything has succeeded we should not call brcmf_sdio_exit anymore. The bus object
  // will be destroyed as part of SdioDevice shutting down.
  on_error.cancel();

  brcmf_bus_ = std::move(bus);
  return ZX_OK;
}

zx_status_t SdioDevice::DeviceAdd(device_add_args_t* args, zx_device_t** out_device) {
  return device_add(parent(), args, out_device);
}

void SdioDevice::DeviceAsyncRemove(zx_device_t* dev) { device_async_remove(dev); }

zx_status_t SdioDevice::LoadFirmware(const char* path, zx_handle_t* fw, size_t* size) {
  return load_firmware(zxdev(), path, fw, size);
}

zx_status_t SdioDevice::DeviceGetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) {
  return device_get_metadata(zxdev(), type, buf, buflen, actual);
}

}  // namespace brcmfmac
}  // namespace wlan
