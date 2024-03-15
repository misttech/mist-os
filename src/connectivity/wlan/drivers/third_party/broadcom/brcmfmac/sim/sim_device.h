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

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_DEVICE_H_

#include <lib/driver/component/cpp/driver_base.h>

#include <memory>

#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/inspect/device_inspect.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_data_path.h"

struct brcmf_bus;

namespace wlan::brcmfmac {

class SimDevice final : public fdf::DriverBase, public Device {
 public:
  SimDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("sim-brcmfmac", std::move(start_args), std::move(driver_dispatcher)) {}

  SimDevice(const SimDevice& device) = delete;
  SimDevice& operator=(const SimDevice& other) = delete;
  ~SimDevice() override;

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

  // Run the simulator bus initialization.  This is a replacement for the DDK init hook.
  zx_status_t SimBusInit();
  zx_status_t BusInit() override { return ZX_OK; }

  // Set the `simulation::Environment` instance that the SimDevice will use.
  // This should be called after `Start()` is called, but before any test logic.
  zx_status_t InitWithEnv(simulation::Environment* env);

  async_dispatcher_t* GetTimerDispatcher() override { return env_->GetDispatcher(); }
  fdf_dispatcher_t* GetDriverDispatcher() override { return driver_dispatcher()->get(); }
  DeviceInspect* GetInspect() override { return inspect_.get(); }
  compat::DeviceServer& GetCompatServer() override { return compat_server_.inner(); }
  fidl::WireClient<fdf::Node>& GetParentNode() override { return parent_node_; }
  std::shared_ptr<fdf::OutgoingDirectory>& Outgoing() override { return outgoing(); }
  const std::shared_ptr<fdf::Namespace>& Incoming() const override { return incoming(); }

  // Trampolines for DDK functions, for platforms that support them.
  zx_status_t LoadFirmware(const char* path, zx_handle_t* fw, size_t* size) override;
  zx_status_t DeviceGetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) override;

  brcmf_simdev* GetSim();

  SimDataPath& DataPath() { return data_path_; }

 protected:
  void Shutdown();

 private:
  void ShutdownImpl();

  simulation::Environment* env_;
  std::unique_ptr<DeviceInspect> inspect_;
  std::unique_ptr<brcmf_bus> brcmf_bus_;

  SimDataPath data_path_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireClient<fdf::Node> parent_node_;
};

}  // namespace wlan::brcmfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_DEVICE_H_
