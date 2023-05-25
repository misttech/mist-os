// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.radar/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>

#include <optional>
#include <vector>

#include <zxtest/zxtest.h>

#include "../radar-proxy.h"
#include "sdk/lib/driver/runtime/testing/cpp/dispatcher.h"

namespace radar {

class FakeRadarDriver : public fidl::Server<fuchsia_hardware_radar::RadarBurstReaderProvider>,
                        public fidl::Server<fuchsia_hardware_radar::RadarBurstReader> {
 public:
  explicit FakeRadarDriver(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void Bind(fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReaderProvider> server) {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&]() {
                  provider_binding_ = fidl::BindServer(dispatcher_, std::move(server), this);
                }).is_ok());
  }

  void UnbindProvider() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&]() {
                  if (provider_binding_) {
                    provider_binding_->Close(ZX_ERR_PEER_CLOSED);
                  }
                }).is_ok());
  }

  void UnbindReader() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&]() {
                  if (reader_binding_) {
                    reader_binding_->Close(ZX_ERR_PEER_CLOSED);
                  }
                }).is_ok());
  }

  void SendBurst() {
    async::PostTask(dispatcher_, [&]() {
      ASSERT_TRUE(reader_binding_);

      for (auto& vmo : registered_vmos_) {
        if (!vmo.locked) {
          vmo.locked = true;
          vmo.vmo.write(&kRealRadarBurstMarker, 0, sizeof(kRealRadarBurstMarker));
          const auto burst = fuchsia_hardware_radar::RadarBurstReaderOnBurst2Request::WithBurst(
              {{vmo.vmo_id, zx::clock::get_monotonic().get()}});
          EXPECT_TRUE(fidl::SendEvent(*reader_binding_)->OnBurst2(burst).is_ok());
          return;
        }
      }

      const auto burst = fuchsia_hardware_radar::RadarBurstReaderOnBurst2Request::WithError(
          fuchsia_hardware_radar::StatusCode::kOutOfVmos);
      EXPECT_TRUE(fidl::SendEvent(*reader_binding_)->OnBurst2(burst).is_ok());
    });
  }

  void SendError(fuchsia_hardware_radar::StatusCode status) {
    async::PostTask(dispatcher_, [&, error = status]() {
      ASSERT_TRUE(reader_binding_);
      const auto burst = fuchsia_hardware_radar::RadarBurstReaderOnBurst2Request::WithError(error);
      EXPECT_TRUE(fidl::SendEvent(*reader_binding_)->OnBurst2(burst).is_ok());
    });
  }

  zx_status_t WaitForBurstsStopped() {
    return sync_completion_wait(&bursts_stopped_, ZX_TIME_INFINITE);
  }

  void set_burst_size(uint32_t burst_size) {
    async::PostTask(dispatcher_, [&, size = burst_size]() { burst_size_ = size; });
  }

 private:
  // We write this marker to the first byte of each burst we send, while injection tests write any
  // other value. This way clients can determine whether received bursts came from the driver or
  // were injected.
  static constexpr uint8_t kRealRadarBurstMarker = 0xff;

  struct RegisteredVmo {
    uint32_t vmo_id;
    zx::vmo vmo;
    bool locked = false;
  };

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    reader_binding_ = fidl::BindServer(dispatcher_, std::move(request.server()), this);
    completer.Reply(fit::ok());
  }

  void GetBurstProperties(GetBurstPropertiesCompleter::Sync& completer) override {
    completer.Reply({burst_size_, zx::usec(1).to_nsecs()});
  }

  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) override {
    if (!registered_vmos_.empty()) {
      // Inject an error on the second RegisterVmos() call to test radar-proxy error handling.
      completer.Reply(fit::error(fuchsia_hardware_radar::StatusCode::kVmoAlreadyRegistered));
      return;
    }
    if (request.vmo_ids().size() != request.vmos().size()) {
      completer.Reply(fit::error(fuchsia_hardware_radar::StatusCode::kInvalidArgs));
      return;
    }

    for (size_t i = 0; i < request.vmos().size(); i++) {
      // Ignore normal registration errors, such as duplicate IDs.
      registered_vmos_.push_back(RegisteredVmo{request.vmo_ids()[i], std::move(request.vmos()[i])});
    }
    completer.Reply(fit::success());
  }

  void UnregisterVmos(UnregisterVmosRequest& request,
                      UnregisterVmosCompleter::Sync& completer) override {}

  void StartBursts(StartBurstsCompleter::Sync& completer) override {
    sync_completion_reset(&bursts_stopped_);
  }

  void StopBursts(StopBurstsCompleter::Sync& completer) override {
    completer.Reply();
    sync_completion_signal(&bursts_stopped_);
  }

  void UnlockVmo(UnlockVmoRequest& request, UnlockVmoCompleter::Sync& completer) override {
    for (auto& vmo : registered_vmos_) {
      if (vmo.vmo_id == request.vmo_id()) {
        vmo.locked = false;
        return;
      }
    }
  }

  async_dispatcher_t* const dispatcher_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReaderProvider>>
      provider_binding_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReader>> reader_binding_;
  std::vector<RegisteredVmo> registered_vmos_;
  sync_completion_t bursts_stopped_;
  uint32_t burst_size_ = 12345;
};

template <typename T>
class RadarProxyTest : public zxtest::Test, public RadarDeviceConnector {
 public:
  RadarProxyTest()
      : proxy_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        driver_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        fake_driver_(driver_loop_.dispatcher()),
        dut_(proxy_loop_.dispatcher(), this) {}

  void SetUp() override {
    ASSERT_OK(proxy_loop_.StartThread("Radar proxy"));
    ASSERT_OK(driver_loop_.StartThread("Radar driver"));

    zx::result endpoints =
        fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
    ASSERT_TRUE(endpoints.is_ok());

    dut_client_.Bind(std::move(endpoints->client));
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  fidl::BindServer(proxy_loop_.dispatcher(), std::move(endpoints->server), &dut_);
                }).is_ok());

    endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
    ASSERT_TRUE(endpoints.is_ok());
  }

  void TearDown() override {
    // Make sure the proxy doesn't try to reconnect after the loop has been stopped.
    UnbindRadarDevice();

    // dut_ must outlive the loop in order to prevent FIDL callbacks from keeping dangling
    // references to them.
    proxy_loop_.Shutdown();
    driver_loop_.Shutdown();
  }

  void ConnectToRadarDevice(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                            const std::string& path,
                            ConnectDeviceCallback connect_device) override {
    ConnectToFirstRadarDevice(std::move(connect_device));
  }

  void ConnectToFirstRadarDevice(ConnectDeviceCallback connect_device) override {
    if (!provider_connect_fail_) {
      zx::result endpoints =
          fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
      ASSERT_TRUE(endpoints.is_ok());

      fake_driver_.Bind(std::move(endpoints->server));
      // This may not succeed if we've told the radar driver to return errors for certain calls.
      connect_device(std::move(endpoints->client));
    }

    sync_completion_signal(&device_connected_);
  }

 private:
  async::Loop proxy_loop_;
  async::Loop driver_loop_;

 protected:
  static fuchsia_hardware_radar::RadarBurstReaderGetBurstPropertiesResponse kInvalidProperties() {
    return fuchsia_hardware_radar::RadarBurstReaderGetBurstPropertiesResponse(0, 0);
  }

  // Closes the RadarBurstProvider connection between radar-proxy and the radar driver, and waits
  // for radar-proxy to process the epitaph. After this call, requests to radar-proxy should either
  // succeed or fail depending on the value of provider_connect_fail_.
  void UnbindProviderAndWaitForConnectionAttempt() {
    sync_completion_reset(&device_connected_);
    UnbindRadarDevice();
    sync_completion_wait(&device_connected_, ZX_TIME_INFINITE);
  }

  // Same as above, but for RadarBurstReader.
  void UnbindReaderAndWaitForConnectionAttempt() {
    sync_completion_reset(&device_connected_);
    fake_driver_.UnbindReader();
    sync_completion_wait(&device_connected_, ZX_TIME_INFINITE);
  }

  void UnbindRadarDevice() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  provider_connect_fail_ = true;
                }).is_ok());
    fake_driver_.UnbindProvider();
  }

  void AddRadarDevice() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  provider_connect_fail_ = false;
                  dut_.DeviceAdded(kInvalidDir, "000");
                }).is_ok());
  }

  void BindInjector(fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstInjector> server_end) {
    dut_.BindInjector(std::move(server_end));
  }

  // Visible for testing.
  FakeRadarDriver fake_driver_;

  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReaderProvider> dut_client_;

 private:
  static constexpr fidl::UnownedClientEnd<fuchsia_io::Directory> kInvalidDir{FIDL_HANDLE_INVALID};

  T dut_;

  bool provider_connect_fail_ = false;
  sync_completion_t device_connected_;
};

}  // namespace radar
