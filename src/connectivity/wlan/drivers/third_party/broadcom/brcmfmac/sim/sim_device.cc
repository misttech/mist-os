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

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_device.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/status.h>

#include <wlan/drivers/log_instance.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/bus.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/debug.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/device.h"

namespace wlan::brcmfmac {

SimDevice::~SimDevice() { ShutdownImpl(); }

void SimDevice::Shutdown() { ShutdownImpl(); }

void SimDevice::ShutdownImpl() {
  // Keep a separate implementation for this that's not virtual so that it can be called from the
  // destructor.
  if (brcmf_bus_) {
    brcmf_sim_exit(brcmf_bus_.get());
    brcmf_bus_.reset();
  }
}

zx::result<> SimDevice::Start() {
  parent_node_.Bind(std::move(node()), dispatcher());
  // Ensure netdevice is initialzied for dfv2?
  CreateNetDevice();
  // synchronously initialize compat_server_
  // Needed by brcmfmac::Device base class.
  zx::result<> result = compat_server_.Initialize(incoming(), outgoing(), node_name(), name(),
                                                  compat::ForwardMetadata::None());
  if (result.is_error()) {
    BRCMF_ERR("Compat server init failed: %s", result.status_string());
    return result.take_error();
  }

  std::unique_ptr<DeviceInspect> inspect;
  zx_status_t status = DeviceInspect::Create(dispatcher(), &inspect);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "ERROR calling DeviceInspect::Create(): %s", zx_status_get_string(status));
    return zx::error(status);
  }

  inspect_ = std::move(inspect);
  wlan::drivers::log::Instance::Init(Debug::kBrcmfMsgFilter);

  status = InitDevice();
  if (status != ZX_OK) {
    lerror("ERROR calling InitDevice(): %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void SimDevice::PrepareStop(fdf::PrepareStopCompleter completer) {
  Shutdown();
  compat_server_.reset();
  completer(zx::ok());
}

zx_status_t SimDevice::InitWithEnv(simulation::Environment* env) {
  env_ = env;
  brcmf_bus_ = brcmf_sim_alloc(drvr(), env);
  return ZX_OK;
}

zx_status_t SimDevice::SimBusInit() {
  zx_status_t status = brcmf_sim_register(drvr());
  if (status != ZX_OK) {
    return status;
  }
  data_path_.Init(NetDev());
  return ZX_OK;
}

zx_status_t SimDevice::LoadFirmware(const char* path, zx_handle_t* fw, size_t* size) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t SimDevice::DeviceGetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

brcmf_simdev* SimDevice::GetSim() { return brcmf_bus_->bus_priv.sim; }

}  // namespace wlan::brcmfmac
FUCHSIA_DRIVER_EXPORT(::wlan::brcmfmac::SimDevice);
