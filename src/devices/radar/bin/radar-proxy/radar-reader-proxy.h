// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_READER_PROXY_H_
#define SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_READER_PROXY_H_

#include <lib/fzl/vmo-mapper.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>

#include <memory>
#include <optional>
#include <tuple>
#include <vector>

#include "radar-proxy.h"
#include "src/devices/radar/lib/vmo-manager/vmo-manager.h"

namespace radar {

class RadarReaderProxy : public RadarProxy,
                         public fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstReader> {
 private:
  class ReaderInstance;

 public:
  RadarReaderProxy(async_dispatcher_t* dispatcher, RadarDeviceConnector* connector)
      : dispatcher_(dispatcher), connector_(connector) {}
  ~RadarReaderProxy() override;

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override;

  void DeviceAdded(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                   const std::string& filename) override;

  void on_fidl_error(fidl::UnbindInfo info) override;

  void OnBurst(fidl::Event<fuchsia_hardware_radar::RadarBurstReader::OnBurst>& event) override;

 private:
  struct MappedVmo {
    fzl::VmoMapper mapped_vmo;
    zx::vmo vmo;
  };

  class ReaderInstance : public fidl::Server<fuchsia_hardware_radar::RadarBurstReader> {
   public:
    ReaderInstance(RadarReaderProxy* parent,
                   fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReader> server_end);

    // RadarBurstReader implementation

    void GetBurstSize(GetBurstSizeCompleter::Sync& completer) override;
    void RegisterVmos(RegisterVmosRequest& request,
                      RegisterVmosCompleter::Sync& completer) override;
    void UnregisterVmos(UnregisterVmosRequest& request,
                        UnregisterVmosCompleter::Sync& completer) override;
    void StartBursts(StartBurstsCompleter::Sync& completer) override;
    void StopBursts(StopBurstsCompleter::Sync& completer) override;
    void UnlockVmo(UnlockVmoRequest& request, UnlockVmoCompleter::Sync& completer) override;

    // Called by RadarReaderProxy

    void SendBurst(cpp20::span<const uint8_t> burst, zx::time timestamp);
    void SendError(fuchsia_hardware_radar::StatusCode error);

    // This causes our on_unbound function to be called, which tells the parent to delete us.
    void Close(zx_status_t status) { server_.Close(status); }

    bool bursts_started() const { return bursts_started_; }

   private:
    RadarReaderProxy* const parent_;
    bool bursts_started_ = false;
    fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReader> server_;
    VmoManager manager_;
    size_t vmo_count_ = 0;
    bool sent_error_ = false;
  };

  bool ValidateDevice(fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstReaderProvider> client_end);

  // Handles an unexpected fatal error that may have gotten us out of sync with the driver. Closes
  // our driver client end, sends an epitaph to all connected clients, and attempts to connect to
  // any other drivers that may be present.
  void HandleFatalError(zx_status_t status);

  // Called by ReaderInstance
  void UpdateVmoCount(size_t count);
  void StartBursts();
  void StopBursts();
  void InstanceUnbound(ReaderInstance* instance);

  async_dispatcher_t* const dispatcher_;
  RadarDeviceConnector* const connector_;
  fidl::Client<fuchsia_hardware_radar::RadarBurstReader> radar_client_;
  std::optional<uint32_t> burst_size_;
  std::vector<MappedVmo> vmo_pool_;
  std::vector<std::unique_ptr<ReaderInstance>> instances_;
  std::vector<std::tuple<fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReader>,
                         ConnectCompleter::Async>>
      connect_requests_;
};

}  // namespace radar

#endif  // SRC_DEVICES_RADAR_BIN_RADAR_PROXY_RADAR_READER_PROXY_H_
