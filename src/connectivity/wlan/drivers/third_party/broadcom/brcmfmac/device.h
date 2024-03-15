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

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_DEVICE_H_

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/fidl.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <memory>
#include <mutex>

#include <wlan/drivers/components/network_device.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/core.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace wlan {
namespace brcmfmac {

class DeviceInspect;
class WlanInterface;
class Device : public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl>,
               public fidl::WireAsyncEventHandler<fdf::NodeController>,
               public ::wlan::drivers::components::NetworkDevice::Callbacks {
 public:
  virtual ~Device();
  explicit Device();

  // Device Initialization
  zx_status_t InitServerDispatcher();
  zx_status_t InitWlanPhyImpl();
  zx_status_t InitDevice();
  void InitPhyDevice();
  void CreateNetDevice();
  virtual zx_status_t BusInit() = 0;
  WlanInterface* GetClientInterface() { return client_interface_.get(); }
  WlanInterface* GetSoftApInterface() { return ap_interface_.get(); }

  // State accessors
  brcmf_pub* drvr();
  const brcmf_pub* drvr() const;
  ::wlan::drivers::components::NetworkDevice* NetDev() { return network_device_.get(); }

  // Virtual state accessors
  virtual async_dispatcher_t* GetTimerDispatcher() = 0;
  virtual DeviceInspect* GetInspect() = 0;
  virtual compat::DeviceServer& GetCompatServer() = 0;
  virtual fidl::WireClient<fdf::Node>& GetParentNode() = 0;
  virtual std::shared_ptr<fdf::OutgoingDirectory>& Outgoing() = 0;
  virtual const std::shared_ptr<fdf::Namespace>& Incoming() const = 0;
  virtual fdf_dispatcher_t* GetDriverDispatcher() = 0;

  // WlanPhyImpl interface implementation.
  void GetSupportedMacRoles(fdf::Arena& arena,
                            GetSupportedMacRolesCompleter::Sync& completer) override;
  void CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                   CreateIfaceCompleter::Sync& completer) override;
  void DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                    DestroyIfaceCompleter::Sync& completer) override;
  void SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                  SetCountryCompleter::Sync& completer) override;
  void GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) override;
  void ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) override;
  void SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                        SetPowerSaveModeCompleter::Sync& completer) override;
  void GetPowerSaveMode(fdf::Arena& arena, GetPowerSaveModeCompleter::Sync& completer) override;

  // NetworkDevice::Callbacks implementation
  void NetDevInit(wlan::drivers::components::NetworkDevice::Callbacks::InitTxn txn) override;
  void NetDevRelease() override;
  void NetDevStart(wlan::drivers::components::NetworkDevice::Callbacks::StartTxn txn) override;
  void NetDevStop(wlan::drivers::components::NetworkDevice::Callbacks::StopTxn txn) override;
  void NetDevGetInfo(device_impl_info_t* out_info) override;
  void NetDevQueueTx(cpp20::span<wlan::drivers::components::Frame> frames) override;
  void NetDevQueueRxSpace(const rx_space_buffer_t* buffers_list, size_t buffers_count,
                          uint8_t* vmo_addrs[]) override;
  zx_status_t NetDevPrepareVmo(uint8_t vmo_id, zx::vmo vmo, uint8_t* mapped_address,
                               size_t mapped_size) override;
  void NetDevReleaseVmo(uint8_t vmo_id) override;
  void NetDevSetSnoopEnabled(bool snoop) override;

  virtual zx_status_t LoadFirmware(const char* path, zx_handle_t* fw, size_t* size) = 0;
  virtual zx_status_t DeviceGetMetadata(uint32_t type, void* buf, size_t buflen,
                                        size_t* actual) = 0;

  // Fidl error handlers
  void on_fidl_error(fidl::UnbindInfo error) override {
    BRCMF_WARN("Fidl Error: %s", error.FormatDescription().c_str());
  }
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {
    BRCMF_WARN("Received unknown event: event_ordinal(%lu)", metadata.event_ordinal);
  }
  // Helper functions
  void DestroyAllIfaces();

 protected:
  // This will be called when the driver is being shut down, for example during a reboot, power off
  // or suspend. It is NOT called as part of destruction of the device object (because calling
  // virtual methods in destructors is unreliable). The device may be subject to multiple stages of
  // shutdown, it is therefore possible for shutdown to be called multiple times. The device object
  // may also be destructed after this as a result of the driver framework calling release. Take
  // care that a multiple shutdowns or a shutdown followed by destruction does not result in double
  // freeing memory or resources. Because this Device class is not Resumable there is no need to
  // worry about coming back from a shutdown state, it's irreversible.
  void Shutdown();

  libsync::Completion completion_;

 private:
  // Helpers
  zx_status_t AddNetworkDevice(const char* deviceName);
  zx_status_t AddWlanPhyImplService();
  void ServiceConnectHandler(fdf_dispatcher_t* dispatcher,
                             fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end);
  void NetDevInitReply(zx_status_t status);

  std::unique_ptr<brcmf_bus> brcmf_bus_;
  std::unique_ptr<brcmf_pub> brcmf_pub_;
  std::mutex lock_;

  // FIDL client of the node that this driver binds to.
  fidl::WireClient<fdf::NodeController> wlanphy_controller_client_;
  fidl::WireClient<fdf::NodeController> wlanfullmac_controller_client_;
  fidl::WireClient<fdf::NodeController> wlanfullmac_controller_softap_;

  std::unique_ptr<::wlan::drivers::components::NetworkDevice> network_device_;
  // Two fixed interfaces supported;  the default interface as a client, and a second one as an AP.
  std::unique_ptr<WlanInterface> client_interface_;
  std::unique_ptr<WlanInterface> ap_interface_;
  fdf::ServerBindingGroup<fuchsia_wlan_phyimpl::WlanPhyImpl> bindings_;
  std::optional<wlan::drivers::components::NetworkDevice::Callbacks::InitTxn> netdev_init_txn_;
};

}  // namespace brcmfmac
}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_DEVICE_H_
