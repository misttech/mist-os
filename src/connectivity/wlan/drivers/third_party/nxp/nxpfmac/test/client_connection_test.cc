// Copyright (c) 2022 The Fuchsia Authors
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

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/client_connection.h"

#include <lib/async/cpp/task.h>
#include <lib/mock-function/mock-function.h>
#include <lib/sync/completion.h>
#include <netinet/ether.h>

#include <memory>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/event_handler.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_adapter.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/key_ring.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mlan_mocks.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mock_bus.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/test_data_plane.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

using wlan::nxpfmac::ClientConnection;
using wlan::nxpfmac::ClientConnectionIfc;
using wlan::nxpfmac::Device;
using wlan::nxpfmac::DeviceContext;

constexpr uint8_t kIesWithSsid[] = {"\x00\x04Test"};
constexpr uint8_t kTestChannel = 6;
constexpr uint32_t kTestBssIndex = 3;
constexpr uint8_t kBss[] = {0x01, 0x02, 0x03, 0x04, 0x05, 0x06};

// Barebones Device Class (at this time mainly for the dispatcher to handle timers)
struct TestDevice : public Device {
 public:
  static zx_status_t Create(zx_device_t *parent, async_dispatcher_t *dispatcher,
                            sync_completion_t *destruction_compl, TestDevice **out_device) {
    auto device = new TestDevice(parent, dispatcher, destruction_compl);
    *out_device = device;
    return ZX_OK;
  }
  ~TestDevice() {}

  async_dispatcher_t *GetDispatcher() override { return dispatcher_; }

 private:
  TestDevice(zx_device_t *parent, async_dispatcher_t *dispatcher,
             sync_completion_t *destructor_done)
      : Device(parent) {
    dispatcher_ = dispatcher;
  }

 protected:
  zx_status_t Init(mlan_device *mlan_dev, wlan::nxpfmac::BusInterface **out_bus) override {
    return ZX_OK;
  }
  zx_status_t LoadFirmware(const char *path, zx::vmo *out_fw, size_t *out_size) override {
    return ZX_OK;
  }
  void Shutdown() override {}

  async_dispatcher_t *dispatcher_;
  wlan::nxpfmac::DeviceContext context_;
  wlan::nxpfmac::MockBus bus_;
};

struct TestClientConnectionIfc : public ClientConnectionIfc {
  void OnDisconnectEvent(uint16_t reason_code) override {}
  void SignalQualityIndication(int8_t rssi, int8_t snr) override {}
  void InitiateSaeHandshake(const uint8_t *peer_addr) override {
    initiate_sae_handshake_.Call(peer_addr);
  }
  void OnAuthFrame(const uint8_t *peer_addr, const wlan::Authentication *auth,
                   cpp20::span<const uint8_t> trailing_data) override {}

  mock_function::MockFunction<void, const uint8_t *> initiate_sae_handshake_;
};

struct TestDataPlaneIfc : public wlan::nxpfmac::DataPlaneIfc {
  void OnEapolTransmitted(wlan::drivers::components::Frame &&frame, zx_status_t status) override {}
  void OnEapolReceived(wlan::drivers::components::Frame &&frame) override {}
};

struct ClientConnectionTest : public zxtest::Test {
  void SetUp() override {
    event_loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigNeverAttachToThread);
    event_loop_->StartThread();
    auto ioctl_adapter = wlan::nxpfmac::IoctlAdapter::Create(mocks_.GetAdapter(), &mock_bus_);
    ASSERT_OK(ioctl_adapter.status_value());
    ioctl_adapter_ = std::move(ioctl_adapter.value());
    key_ring_ = std::make_unique<wlan::nxpfmac::KeyRing>(ioctl_adapter_.get(), kTestBssIndex);
    // TODO(fxb/124464): Migrate test to use dispatcher integration.
    parent_ = MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED();
    ASSERT_OK(TestDevice::Create(parent_.get(), env_.GetDispatcher(), &device_destructed_,
                                 &test_device_));
    ASSERT_OK(wlan::nxpfmac::TestDataPlane::Create(&data_plane_ifc_, &mock_bus_,
                                                   mocks_.GetAdapter(), &test_data_plane_));

    context_ = wlan::nxpfmac::DeviceContext{.device_ = test_device_,
                                            .event_handler_ = &event_handler_,
                                            .ioctl_adapter_ = ioctl_adapter_.get(),
                                            .data_plane_ = test_data_plane_->GetDataPlane()};

    auto builder =
        fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest::Builder(request_arena_);

    fuchsia_wlan_internal::wire::BssDescription bss;
    memcpy(bss.bssid.data(), kBss, sizeof(kBss));
    bss.ies = fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t *>(kIesWithSsid),
                                                      sizeof(kIesWithSsid));
    bss.channel.primary = kTestChannel;

    builder.selected_bss(bss);
    builder.auth_type(fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem);
    default_request_ = builder.Build();
  }

  void TearDown() override {
    delete context_.device_;
    context_.device_ = nullptr;
    event_loop_->Shutdown();
  }

  mlan_status HandleOtherIoctls(pmlan_ioctl_req req) {
    if (req->req_id == MLAN_IOCTL_SEC_CFG || req->req_id == MLAN_IOCTL_MISC_CFG ||
        req->req_id == MLAN_IOCTL_GET_INFO) {
      // Connecting performs security and IE (MISC ioctl) configuration, make sure it succeeds.
      return MLAN_STATUS_SUCCESS;
    }
    if (req->req_id == MLAN_IOCTL_SCAN) {
      // For scans we must send a scan report to continue the scanning process.
      zxlogf(INFO, "Posting event");
      async::PostTask(event_loop_->dispatcher(), [this]() {
        zxlogf(INFO, "Sending event");
        mlan_event event{.event_id = MLAN_EVENT_ID_DRV_SCAN_REPORT};
        event_handler_.OnEvent(&event);
      });
      return MLAN_STATUS_SUCCESS;
    }
    if (req->req_id == MLAN_IOCTL_BSS) {
      auto request = reinterpret_cast<wlan::nxpfmac::IoctlRequest<mlan_ds_bss> *>(req);
      if (request->UserReq().sub_command == MLAN_OID_BSS_CHANNEL_LIST) {
        constexpr uint8_t kChannels[] = {1,   2,   3,   4,   5,   6,   7,   8,   9,
                                         10,  11,  36,  40,  44,  48,  52,  56,  60,
                                         64,  100, 104, 108, 112, 116, 120, 124, 128,
                                         132, 136, 140, 144, 149, 153, 157, 161, 165};
        for (size_t i = 0; i < std::size(kChannels); ++i) {
          request->UserReq().param.chanlist.cf[i].channel = kChannels[i];
        }
        request->UserReq().param.chanlist.num_of_chan = std::size(kChannels);

        return MLAN_STATUS_SUCCESS;
      }
    }
    // Unexpected
    ADD_FAILURE("Should not reach this point, unexpected ioctl 0x%x", req->req_id);
    return MLAN_STATUS_FAILURE;
  }

  fidl::Arena<wlan::nxpfmac::kConnectReqBufferSize> request_arena_;
  fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest default_request_;
  std::unique_ptr<async::Loop> event_loop_;
  wlan::simulation::Environment env_ = {};
  TestDevice *test_device_ = nullptr;
  wlan::nxpfmac::MlanMockAdapter mocks_;
  wlan::nxpfmac::MockBus mock_bus_;
  wlan::nxpfmac::EventHandler event_handler_;
  wlan::nxpfmac::DeviceContext context_;
  std::unique_ptr<wlan::nxpfmac::IoctlAdapter> ioctl_adapter_;
  std::unique_ptr<wlan::nxpfmac::KeyRing> key_ring_;
  TestDataPlaneIfc data_plane_ifc_;
  std::unique_ptr<wlan::nxpfmac::TestDataPlane> test_data_plane_;
  TestClientConnectionIfc test_ifc_;
  sync_completion_t device_destructed_;
  std::shared_ptr<MockDevice> parent_;
};

TEST_F(ClientConnectionTest, Constructible) {
  ASSERT_NO_FATAL_FAILURE(ClientConnection(&test_ifc_, &context_, nullptr, 0));
}

TEST_F(ClientConnectionTest, Connect) {
  constexpr uint8_t kStaAddr[] = {0x0e, 0x0d, 0x16, 0x28, 0x3a, 0x4c};
  constexpr uint32_t kBssIndex = 0;
  constexpr int8_t kTestRssi = -64;
  constexpr int8_t kTestSnr = 28;
  constexpr zx::duration kSignalLogTimeout = zx::sec(30);

  sync_completion_t ioctl_completion;

  class TestClientConnectionIfc : public ClientConnectionIfc {
    void OnDisconnectEvent(uint16_t reason_code) override {}
    void SignalQualityIndication(int8_t rssi, int8_t snr) override {
      ind_rssi = rssi;
      ind_snr = snr;
    }
    void InitiateSaeHandshake(const uint8_t *peer_addr) override {}
    void OnAuthFrame(const uint8_t *peer_addr, const wlan::Authentication *auth,
                     cpp20::span<const uint8_t> trailing_data) override {}

   public:
    int8_t get_ind_rssi() { return ind_rssi; }

    int8_t get_ind_snr() { return ind_snr; }

    int8_t ind_rssi = 0;
    int8_t ind_snr = 0;
  };

  TestClientConnectionIfc test_ifc;
  sync_completion_t signal_ioctl_received;
  std::atomic<bool> ies_cleared;
  auto on_ioctl = [&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss *>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_START) {
        // This is the actual connect ioctl.

        auto request = reinterpret_cast<wlan::nxpfmac::IoctlRequest<mlan_ds_bss> *>(req);
        auto &user_req = request->UserReq();
        EXPECT_EQ(MLAN_IOCTL_BSS, request->IoctlReq().req_id);
        EXPECT_EQ(MLAN_ACT_SET, request->IoctlReq().action);

        // EXPECT_EQ(MLAN_OID_BSS_START, user_req.sub_command);
        EXPECT_EQ(kBssIndex, user_req.param.ssid_bssid.idx);
        EXPECT_EQ(kTestChannel, user_req.param.ssid_bssid.channel);
        EXPECT_BYTES_EQ(kBss, user_req.param.ssid_bssid.bssid, ETH_ALEN);

        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);

        sync_completion_signal(&ioctl_completion);
        return MLAN_STATUS_PENDING;
      }
      if (bss->sub_command == MLAN_OID_BSS_STOP) {
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
    } else if (req->action == MLAN_ACT_GET && req->req_id == MLAN_IOCTL_GET_INFO) {
      auto signal_info = reinterpret_cast<mlan_ds_get_info *>(req->pbuf);
      if (signal_info->sub_command == MLAN_OID_GET_SIGNAL) {
        signal_info->param.signal.data_snr_avg = kTestSnr;
        signal_info->param.signal.data_rssi_avg = kTestRssi;
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        sync_completion_signal(&signal_ioctl_received);
        return MLAN_STATUS_PENDING;
      }
    } else if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_MISC_CFG) {
      auto misc_cfg = reinterpret_cast<mlan_ds_misc_cfg *>(req->pbuf);
      if (misc_cfg->sub_command == MLAN_OID_MISC_GEN_IE) {
        if (misc_cfg->param.gen_ie.type == MLAN_IE_TYPE_GEN_IE && misc_cfg->param.gen_ie.len == 0) {
          // This is an ioctl to clear out any existing IEs.
          ies_cleared = true;
        }
      }
    }
    return HandleOtherIoctls(req);
  };

  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  ClientConnection connection(&test_ifc, &context_, key_ring_.get(), kBssIndex);

  sync_completion_t connect_completion;
  auto on_connect = [&](ClientConnection::StatusCode status_code, const uint8_t *ies,
                        size_t ies_size) {
    EXPECT_EQ(ClientConnection::StatusCode::kSuccess, status_code);
    sync_completion_signal(&connect_completion);
  };

  ASSERT_OK(connection.Connect(&default_request_, std::move(on_connect)));

  ASSERT_OK(sync_completion_wait(&ioctl_completion, ZX_TIME_INFINITE));
  ASSERT_OK(sync_completion_wait(&connect_completion, ZX_TIME_INFINITE));

  // Wait until the timer has been scheduled.
  while (1) {
    if (env_.GetLatestEventTime().get() >= kSignalLogTimeout.to_nsecs()) {
      break;
    }
  }
  // Let the timer run
  env_.Run(kSignalLogTimeout);
  ASSERT_OK(sync_completion_wait(&signal_ioctl_received, ZX_TIME_INFINITE));
  // Ensure the Signal quality indication was called.
  EXPECT_EQ(test_ifc.ind_rssi, kTestRssi);
  EXPECT_EQ(test_ifc.ind_snr, kTestSnr);
  // Expect that each connect call will clear out any existing IEs first.
  EXPECT_TRUE(ies_cleared.load());

  ASSERT_OK(connection.Disconnect(kStaAddr, 0, [&](wlan::nxpfmac::IoctlStatus status) {
    EXPECT_EQ(wlan::nxpfmac::IoctlStatus::Success, status);
  }));
}

TEST_F(ClientConnectionTest, CancelConnect) {
  constexpr uint32_t kBssIndex = 0;

  auto on_ioctl = [&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss *>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_START) {
        // This is the actual connect call, don't indicate ioctl complete and return pending so
        // that the connect attempt is never completed, allowing us to cancel it with certainty.
        return MLAN_STATUS_PENDING;
      }
    }
    if (req->action == MLAN_ACT_CANCEL) {
      // The cancelation also needs to set the status code and call ioctl complete with a failure.
      req->status_code = MLAN_ERROR_CMD_CANCEL;
      ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Failure);
      return MLAN_STATUS_SUCCESS;
    }
    return HandleOtherIoctls(req);
  };

  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kBssIndex);

  sync_completion_t completion;
  ASSERT_OK(connection.Connect(
      &default_request_, [&](ClientConnection::StatusCode status_code, const uint8_t *, size_t) {
        EXPECT_EQ(ClientConnection::StatusCode::kCanceled, status_code);
        sync_completion_signal(&completion);
      }));

  ASSERT_OK(connection.CancelConnect());
  ASSERT_OK(sync_completion_wait(&completion, ZX_TIME_INFINITE));
}

TEST_F(ClientConnectionTest, Disconnect) {
  // Test that disconnecting works as expected.
  constexpr uint8_t kStaAddr[] = {0x02, 0x04, 0x06, 0x08, 0x0a, 0x0c};
  constexpr uint16_t kReasonCode = 12;

  std::atomic<int> connect_calls = 0;
  std::atomic<int> disconnect_calls = 0;
  auto on_ioctl = [&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss *>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_START) {
        // This is the connect call. Complete it asynchronously.
        ++connect_calls;
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
      if (bss->sub_command == MLAN_OID_BSS_STOP) {
        // This is the cancel call. Complete it asynchronously.
        ++disconnect_calls;
        // Make sure the reason code propagated correctly.
        EXPECT_EQ(kReasonCode,
                  reinterpret_cast<const mlan_ds_bss *>(req->pbuf)->param.deauth_param.reason_code);
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
    }
    return HandleOtherIoctls(req);
  };
  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kTestBssIndex);

  // First ensure that if we're not connected we can't disconnect.
  ASSERT_EQ(ZX_ERR_NOT_CONNECTED, connection.Disconnect(kStaAddr, kReasonCode, [](auto) {}));

  sync_completion_t connect_completion;
  auto on_connect = [&](ClientConnection::StatusCode status, const uint8_t *ies, size_t ies_len) {
    EXPECT_EQ(ClientConnection::StatusCode::kSuccess, status);
    sync_completion_signal(&connect_completion);
  };

  // Now connect so that we can successfully disconnect
  ASSERT_OK(connection.Connect(&default_request_, std::move(on_connect)));
  ASSERT_OK(sync_completion_wait(&connect_completion, ZX_TIME_INFINITE));

  // Disconnecting should now work.
  sync_completion_t disconnect_completion;
  ASSERT_OK(connection.Disconnect(kStaAddr, kReasonCode, [&](wlan::nxpfmac::IoctlStatus status) {
    EXPECT_EQ(wlan::nxpfmac::IoctlStatus::Success, status);
    sync_completion_signal(&disconnect_completion);
  }));

  sync_completion_wait(&disconnect_completion, ZX_TIME_INFINITE);

  // Now that we're successfully disconnected make sure disconnect fails again.
  ASSERT_EQ(ZX_ERR_NOT_CONNECTED, connection.Disconnect(kStaAddr, kReasonCode, [](auto) {}));

  // These calls should only have happened once
  ASSERT_EQ(1u, connect_calls.load());
  ASSERT_EQ(1u, disconnect_calls.load());
}

TEST_F(ClientConnectionTest, DisconnectOnDestruct) {
  // Test that Disconnect is called when a connection object is destroyed.

  constexpr uint16_t kReasonCode = REASON_CODE_LEAVING_NETWORK_DEAUTH;

  std::atomic<bool> disconnect_called = false;
  auto on_ioctl = [&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss *>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_START) {
        // This is the connect call. Complete it asynchronously.
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
      if (bss->sub_command == MLAN_OID_BSS_STOP) {
        // This is the cancel call. Complete it asynchronously.
        // Make sure the correct reason code is used.
        auto &deauth = reinterpret_cast<const mlan_ds_bss *>(req->pbuf)->param.deauth_param;
        EXPECT_EQ(kReasonCode, deauth.reason_code);
        // And that an empty MAC address was indicated.
        EXPECT_BYTES_EQ("\0\0\0\0\0\0", deauth.mac_addr, ETH_ALEN);
        disconnect_called = true;
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
    }
    return HandleOtherIoctls(req);
  };
  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  {
    ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kTestBssIndex);

    sync_completion_t connect_completion;
    auto on_connect = [&](ClientConnection::StatusCode status, const uint8_t *ies, size_t ies_len) {
      EXPECT_EQ(ClientConnection::StatusCode::kSuccess, status);
      sync_completion_signal(&connect_completion);
    };

    // First we connect so that we can successfully disconnect
    ASSERT_OK(connection.Connect(&default_request_, std::move(on_connect)));
    ASSERT_OK(sync_completion_wait(&connect_completion, ZX_TIME_INFINITE));
  }
  // The ClientConnection object has now gone out of scope and should have disconnected as part of
  // being destroyed.
  ASSERT_TRUE(disconnect_called.load());
}

TEST_F(ClientConnectionTest, DisconnectAsyncFailure) {
  // Test that if the disconnect ioctl fails asynchronously we get the correct status code.
  constexpr uint8_t kStaAddr[] = {0x0e, 0x0d, 0x16, 0x28, 0x3a, 0x4c};
  constexpr uint16_t kReasonCode = 2;

  std::atomic<bool> fail_disconnect = true;
  auto on_ioctl = [&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss *>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_START) {
        // This is the connect call. Complete it asynchronously.
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
      if (bss->sub_command == MLAN_OID_BSS_STOP) {
        // This is the disconnect call. Fail it asynchronously.
        if (fail_disconnect) {
          ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Failure);
        } else {
          ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        }
        return MLAN_STATUS_PENDING;
      }
    }
    return HandleOtherIoctls(req);
  };
  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kTestBssIndex);

  sync_completion_t connect_completion;
  auto on_connect = [&](ClientConnection::StatusCode status, const uint8_t *ies, size_t ies_len) {
    EXPECT_EQ(ClientConnection::StatusCode::kSuccess, status);
    sync_completion_signal(&connect_completion);
  };

  // Now connect so that we can successfully disconnect
  ASSERT_OK(connection.Connect(&default_request_, std::move(on_connect)));
  ASSERT_OK(sync_completion_wait(&connect_completion, ZX_TIME_INFINITE));

  sync_completion_t disconnect_completion;
  ASSERT_OK(connection.Disconnect(kStaAddr, kReasonCode, [&](wlan::nxpfmac::IoctlStatus status) {
    EXPECT_EQ(wlan::nxpfmac::IoctlStatus::Failure, status);
    sync_completion_signal(&disconnect_completion);
  }));
  ASSERT_OK(sync_completion_wait(&disconnect_completion, ZX_TIME_INFINITE));
  // Allows the disconnect in the destruction of ClientConnection to work.
  fail_disconnect = false;
}

TEST_F(ClientConnectionTest, DisconnectWhileDisconnectInProgress) {
  // Test that an attempt to disconnect is refused while another disconnect is in progress.

  constexpr uint8_t kStaAddr[] = {0x0e, 0x0d, 0x16, 0x28, 0x3a, 0x4c};
  constexpr uint16_t kReasonCode = 2;

  pmlan_ioctl_req disconnect_request = nullptr;
  sync_completion_t disconnect_request_received;
  auto on_ioctl = [&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss *>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_START) {
        // This is the connect call. Complete it asynchronously.
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
      if (bss->sub_command == MLAN_OID_BSS_STOP) {
        // This is the disconnect call. Fail it asynchronously. Don't complete it, leave it hanging
        // so the second disconnect attempt fails because another one is in progress.
        disconnect_request = req;
        sync_completion_signal(&disconnect_request_received);
        return MLAN_STATUS_PENDING;
      }
    }
    return HandleOtherIoctls(req);
  };
  mocks_.SetOnMlanIoctl(std::move(on_ioctl));

  ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kTestBssIndex);

  sync_completion_t connect_completion;
  auto on_connect = [&](ClientConnection::StatusCode status, const uint8_t *ies, size_t ies_len) {
    EXPECT_EQ(ClientConnection::StatusCode::kSuccess, status);
    sync_completion_signal(&connect_completion);
  };

  // Now connect so that we can successfully disconnect
  ASSERT_OK(connection.Connect(&default_request_, std::move(on_connect)));
  ASSERT_OK(sync_completion_wait(&connect_completion, ZX_TIME_INFINITE));

  ASSERT_OK(connection.Disconnect(kStaAddr, kReasonCode, [&](wlan::nxpfmac::IoctlStatus status) {
    EXPECT_EQ(wlan::nxpfmac::IoctlStatus::Success, status);
  }));

  ASSERT_EQ(ZX_ERR_ALREADY_EXISTS, connection.Disconnect(kStaAddr, kReasonCode, [](auto) {}));

  // Now make sure that the first disconnect request was received, then complete it so the
  // connection can be destroyed.
  sync_completion_wait(&disconnect_request_received, ZX_TIME_INFINITE);
  ioctl_adapter_->OnIoctlComplete(disconnect_request, wlan::nxpfmac::IoctlStatus::Success);
}

TEST_F(ClientConnectionTest, InitiateSaeHandshake) {
  // Test that if a WPA3/SAE connect call is made then the connection will request a handshake.

  // Create a request with a specific BSSID and SAE auth method, this should trigger the SAE
  // handshake request.
  fidl::Arena arena;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest::Builder(arena);

  fuchsia_wlan_internal::wire::BssDescription bss;
  memcpy(bss.bssid.data(), kBss, sizeof(kBss));
  builder.selected_bss(bss);
  builder.auth_type(fuchsia_wlan_fullmac::wire::WlanAuthType::kSae);
  auto req = builder.Build();

  test_ifc_.initiate_sae_handshake_.ExpectCall(req.selected_bss().bssid.data());

  ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kTestBssIndex);

  ASSERT_OK(connection.Connect(&req, [](ClientConnection::StatusCode, const uint8_t *, size_t) {}));

  test_ifc_.initiate_sae_handshake_.VerifyAndClear();
}

TEST_F(ClientConnectionTest, TransmitAuthFrame) {
  // Test that calling TransmitAuthFrame works

  constexpr uint8_t kChannel = 10;

  struct AuthFrame {
    uint32_t type;
    uint32_t tx_ctrl;
    uint16_t len;
    IEEE80211_MGMT mgmt;
  } __PACKED;

  std::vector<pmlan_buffer> sent_buffers;

  mocks_.SetOnMlanSendPacket([&](t_void *, pmlan_buffer buffer) -> mlan_status {
    sent_buffers.push_back(buffer);
    return MLAN_STATUS_PENDING;
  });

  std::optional<uint8_t> remain_on_channel;
  std::optional<uint32_t> subtype_mask;
  mocks_.SetOnMlanIoctl([&](t_void *, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_RADIO_CFG) {
      auto radio_cfg = reinterpret_cast<const mlan_ds_radio_cfg *>(req->pbuf);
      if (radio_cfg->sub_command == MLAN_OID_REMAIN_CHAN_CFG) {
        remain_on_channel = radio_cfg->param.remain_chan.channel;
        return MLAN_STATUS_SUCCESS;
      }
    } else if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_MISC_CFG) {
      auto misc_cfg = reinterpret_cast<const mlan_ds_misc_cfg *>(req->pbuf);
      if (misc_cfg->sub_command == MLAN_OID_MISC_RX_MGMT_IND) {
        subtype_mask = misc_cfg->param.mgmt_subtype_mask;
        return MLAN_STATUS_SUCCESS;
      }
    } else if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_SEC_CFG) {
      auto sec_cfg = reinterpret_cast<const mlan_ds_sec_cfg *>(req->pbuf);
      if (sec_cfg->sub_command == MLAN_OID_SEC_CFG_ENCRYPT_KEY) {
        // Destroying the test fixture destroys the key ring, which in turn removes all keys. Allow
        // that ioctl to succeed.
        return MLAN_STATUS_SUCCESS;
      }
    }
    ADD_FAILURE("Unexpected ioctl 0x%x", req->req_id);
    return MLAN_STATUS_FAILURE;
  });

  ClientConnection connection(&test_ifc_, &context_, key_ring_.get(), kTestBssIndex);

  // We need to connect first, there has to be a stored connection request for the auth frame
  // transmission to work and the connection has to be in the correct state.
  fidl::Arena arena;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest::Builder(arena);

  fuchsia_wlan_internal::wire::BssDescription bss;
  memcpy(bss.bssid.data(), kBss, sizeof(kBss));
  bss.channel.primary = kChannel;
  builder.selected_bss(bss);
  builder.auth_type(fuchsia_wlan_fullmac::wire::WlanAuthType::kSae);
  auto req = builder.Build();

  test_ifc_.initiate_sae_handshake_.ExpectCall(req.selected_bss().bssid.data());
  ASSERT_OK(connection.Connect(&req, [](ClientConnection::StatusCode, const uint8_t *, size_t) {}));
  test_ifc_.initiate_sae_handshake_.VerifyAndClear();

  // Initiating an SAE connection sould register for auth,deauth,disassoc management frames
  ASSERT_TRUE(subtype_mask.has_value());
  constexpr uint32_t kExpectedSubtypeMask = (1 << wlan::ManagementSubtype::kAuthentication) |
                                            (1 << wlan::ManagementSubtype::kDeauthentication) |
                                            (1 << wlan::ManagementSubtype::kDisassociation);
  EXPECT_EQ(kExpectedSubtypeMask, subtype_mask.value());

  const uint8_t source[ETH_ALEN] = {1, 2, 3, 4, 5, 6};
  const uint8_t dest[ETH_ALEN]{11, 12, 13, 14, 15, 16};

  constexpr uint16_t kSequence = 1;
  constexpr uint16_t kStatusCode = STATUS_CODE_SUCCESS;
  constexpr uint8_t kSaeFields[] = {20, 21, 22, 23, 24, 25, 26, 27, 28, 19};

  ASSERT_OK(connection.TransmitAuthFrame(source, dest, kSequence, kStatusCode,
                                         cpp20::span<const uint8_t>(kSaeFields)));

  ASSERT_EQ(1u, sent_buffers.size());

  // Because IEEE80211_MGMT contains a union of all possible frame types we need to do some trickery
  // to figure out the size here.
  constexpr size_t kMinimumSize = offsetof(AuthFrame, mgmt) + offsetof(IEEE80211_MGMT, u) +
                                  sizeof(IEEEtypes_Auth_framebody) + sizeof(kSaeFields);

  pmlan_buffer buffer = sent_buffers[0];
  ASSERT_GE(buffer->data_len, kMinimumSize);
  auto auth = reinterpret_cast<const AuthFrame *>(buffer->pbuf + buffer->data_offset);

  EXPECT_EQ(PKT_TYPE_MGMT_FRAME, auth->type);
  // Should be an auth frame
  EXPECT_EQ(wlan::kManagement | (wlan::kAuthentication << 4), auth->mgmt.frame_control);
  EXPECT_BYTES_EQ(source, auth->mgmt.sa, ETH_ALEN);
  EXPECT_BYTES_EQ(dest, auth->mgmt.da, ETH_ALEN);
  EXPECT_BYTES_EQ(dest, auth->mgmt.bssid, ETH_ALEN);
  // Addr4 is expected to be the broadcast MAC address.
  EXPECT_BYTES_EQ("\xFF\xFF\xFF\xFF\xFF\xFF", auth->mgmt.addr4, ETH_ALEN);

  EXPECT_EQ(wlan::kSae, auth->mgmt.u.auth.auth_alg);
  EXPECT_EQ(kStatusCode, auth->mgmt.u.auth.status_code);
  EXPECT_EQ(kSequence, auth->mgmt.u.auth.auth_transaction);

  EXPECT_EQ(MLAN_BUF_TYPE_RAW_DATA, buffer->buf_type);

  EXPECT_BYTES_EQ(kSaeFields, auth->mgmt.u.auth.variable, sizeof(kSaeFields));

  // Sending the frame should have sent a remain on channel ioctl.
  ASSERT_TRUE(remain_on_channel.has_value());
  EXPECT_EQ(kChannel, remain_on_channel.value());
}

}  // namespace
