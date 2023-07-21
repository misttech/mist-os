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

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/wlan_interface.h"

#include <lib/async/cpp/task.h>
#include <lib/fdio/directory.h>
#include <stdlib.h>

#include <fbl/string_buffer.h>
#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/data_plane.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_adapter.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mlan_mocks.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mock_bus.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/test_data_plane.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

using wlan::nxpfmac::DeviceContext;
using wlan::nxpfmac::WlanInterface;

namespace {

constexpr char kFullmacClientDeviceName[] = "test-client-fullmac-ifc";
constexpr char kFullmacSoftApDeviceName[] = "test-softap-fullmac-ifc";
constexpr uint8_t kClientMacAddress[] = {0x00, 0x01, 0x02, 0x03, 0x04, 0x05};
constexpr uint8_t kSoftApMacAddress[] = {0x05, 0x04, 0x03, 0x02, 0x01, 0x00};

using wlan::nxpfmac::Device;

// Barebones Device Class (at this time mainly for the dispatcher to handle timers)
struct TestDevice : public Device {
 public:
  static zx_status_t Create(zx_device_t* parent, async_dispatcher_t* dispatcher,
                            TestDevice** out_device) {
    auto device = new TestDevice(parent, dispatcher);

    *out_device = device;
    return ZX_OK;
  }

  ~TestDevice() {}

  zx_status_t CreateInterface(zx_device_t* parent, const char* name, uint32_t iface_index,
                              fuchsia_wlan_common::WlanMacRole role, DeviceContext* context,
                              const uint8_t mac_address[ETH_ALEN], zx::channel&& mlme_channel) {
    zx_status_t status = WlanInterface::Create(parent, iface_index, role, context, mac_address,
                                               std::move(mlme_channel), &ifc_);

    ifc_->DdkAdd("test-nxpfmac-interface", DEVICE_ADD_NON_BINDABLE);
    // The device will not be actually removed until mock_ddk::ReleaseFlaggedDevices, this only sets
    // the device to a releasable state.
    ifc_->DdkAsyncRemove();
    return status;
  }

  zx_status_t ServeIfaceProtocol(fidl::ServerEnd<fuchsia_io::Directory>&& server_end) {
    return ifc_->ServeWlanFullmacImplProtocol(std::move(server_end));
  }

  WlanInterface* GetInterface() { return ifc_; }

  async_dispatcher_t* GetDispatcher() override { return dispatcher_; }

 private:
  TestDevice(zx_device_t* parent, async_dispatcher_t* dispatcher) : Device(parent) {
    dispatcher_ = dispatcher;
  }

 protected:
  zx_status_t Init(mlan_device* mlan_dev, wlan::nxpfmac::BusInterface** out_bus) override {
    return ZX_OK;
  }
  zx_status_t LoadFirmware(const char* path, zx::vmo* out_fw, size_t* out_size) override {
    return ZX_OK;
  }
  void Shutdown() override {}

  async_dispatcher_t* dispatcher_;
  WlanInterface* ifc_ = nullptr;
  DeviceContext context_;
  wlan::nxpfmac::MockBus bus_;

 public:
};

struct WlanInterfaceTest : public zxtest::Test,
                           public wlan::nxpfmac::DataPlaneIfc,
                           public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImplIfc> {
  WlanInterfaceTest() : test_arena_(nullptr) {}

  void SetUp() override {
    // TODO(fxb/124464): Migrate test to use dispatcher integration.
    parent_ = MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED();
    ASSERT_OK(wlan::nxpfmac::TestDataPlane::Create(this, &mock_bus_, mlan_mocks_.GetAdapter(),
                                                   &test_data_plane_));

    // The last device added right after creating the dataplane should be the network device.
    SetupNetDevice(test_data_plane_->GetNetDevice());

    auto ioctl_adapter = wlan::nxpfmac::IoctlAdapter::Create(mlan_mocks_.GetAdapter(), &mock_bus_);
    ASSERT_OK(ioctl_adapter.status_value());
    ioctl_adapter_ = std::move(ioctl_adapter.value());
    dispatcher_loop_ = std::make_unique<::async::Loop>(&kAsyncLoopConfigNeverAttachToThread);
    ASSERT_OK(dispatcher_loop_->StartThread());
    ASSERT_OK(TestDevice::Create(parent_.get(), env_.GetDispatcher(), &test_device_));
    context_.event_handler_ = &event_handler_;
    context_.ioctl_adapter_ = ioctl_adapter_.get();
    context_.data_plane_ = test_data_plane_->GetDataPlane();
    context_.device_ = test_device_;

    auto driver_dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "test-driver-dispatcher",
        [&](fdf_dispatcher_t*) { driver_completion_.Signal(); });
    ASSERT_FALSE(driver_dispatcher.is_error());
    driver_dispatcher_ = *std::move(driver_dispatcher);

    auto server_dispatcher = fdf::SynchronizedDispatcher::Create(
        {}, "test-server-dispatcher", [&](fdf_dispatcher_t*) { server_completion_.Signal(); });
    ASSERT_FALSE(driver_dispatcher.is_error());
    server_dispatcher_ = *std::move(server_dispatcher);
  }

  void TearDown() override {
    test_data_plane_.reset();
    mock_ddk::ReleaseFlaggedDevices(parent_.get(), driver_dispatcher_.async_dispatcher());

    driver_dispatcher_.ShutdownAsync();
    driver_completion_.Wait();

    server_dispatcher_.ShutdownAsync();
    server_completion_.Wait();
    // Destroy the dataplane before the mock device. This ensures a safe destruction before the
    // parent device of the NetworkDeviceImpl device goes away.
    delete context_.device_;
    context_.device_ = nullptr;
  }

  zx_status_t CreateDeviceInterface(zx_device_t* parent, const char* name, uint32_t iface_index,
                                    fuchsia_wlan_common::wire::WlanMacRole role,
                                    DeviceContext* context, const uint8_t mac_address[ETH_ALEN],
                                    zx::channel&& mlme_channel) {
    libsync::Completion created;
    async::PostTask(driver_dispatcher_.async_dispatcher(), [&]() {
      ASSERT_EQ(ZX_OK, test_device_->CreateInterface(parent, name, iface_index, role, context,
                                                     mac_address, std::move(mlme_channel)));
      created.Signal();
    });
    created.Wait();

    auto outgoing_dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_FALSE(outgoing_dir_endpoints.is_error());

    // `ServeWlanFullmacImplProtocol` must be called in a driver dispatcher because
    // it manipulates a `driver::OutgoingDirectory` which can only be accessed
    // on the same fdf dispatcher that created it.
    libsync::Completion served;
    async::PostTask(driver_dispatcher_.async_dispatcher(), [&]() {
      ASSERT_EQ(ZX_OK, test_device_->ServeIfaceProtocol(std::move(outgoing_dir_endpoints->server)));
      served.Signal();
    });
    served.Wait();

    // Connect to the WlanFullmacImpl protocol found in the interface device's outgoing
    // directory.
    {
      auto endpoints =
          fdf::CreateEndpoints<fuchsia_wlan_fullmac::Service::WlanFullmacImpl::ProtocolType>();
      EXPECT_FALSE(endpoints.is_error());
      zx::channel client_token, server_token;
      EXPECT_EQ(ZX_OK, zx::channel::create(0, &client_token, &server_token));
      EXPECT_EQ(ZX_OK,
                fdf::ProtocolConnect(std::move(client_token),
                                     fdf::Channel(endpoints->server.TakeChannel().release())));
      fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
      path.AppendPrintf("svc/%s/default/%s",
                        fuchsia_wlan_fullmac::Service::WlanFullmacImpl::ServiceName,
                        fuchsia_wlan_fullmac::Service::WlanFullmacImpl::Name);
      // Serve the WlanFullmacImpl protocol on `server_token` found at `path` within
      // the outgoing directory.
      EXPECT_EQ(ZX_OK, fdio_service_connect_at(outgoing_dir_endpoints->client.channel().get(),
                                               path.c_str(), server_token.release()));
      client_ =
          fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl>(std::move(endpoints->client));
    }

    // Create test arena.
    auto arena = fdf::Arena::Create(0, 0);
    EXPECT_FALSE(arena.is_error());

    test_arena_ = *std::move(arena);
    return ZX_OK;
  }

  WlanInterface* GetInterface() { return test_device_->GetInterface(); }

  void SetupNetDevice(zx_device* net_device) {
    // Because WlanInterface creates a port for the netdevice we need to provide a limited
    // implementation of the netdevice ifc.
    network_device_impl_protocol_t netdev_proto;
    ASSERT_OK(device_get_protocol(net_device, ZX_PROTOCOL_NETWORK_DEVICE_IMPL, &netdev_proto));
    ASSERT_OK(
        network_device_impl_init(&netdev_proto, netdev_ifc_proto_.ctx, netdev_ifc_proto_.ops));
  }

  static void OnAddPort(void* ctx, uint8_t, const network_port_protocol_t* proto) {
    auto ifc = static_cast<WlanInterfaceTest*>(ctx);
    ifc->net_port_proto_ = *proto;
    EXPECT_NOT_NULL(proto->ctx);
    EXPECT_NOT_NULL(proto->ops);
    sync_completion_signal(&ifc->on_add_port_called_);
  }
  static void OnRemovePort(void* ctx, uint8_t) {
    auto ifc = static_cast<WlanInterfaceTest*>(ctx);
    sync_completion_signal(&ifc->on_remove_port_called_);
    if (ifc->net_port_proto_.ctx && ifc->net_port_proto_.ops) {
      network_port_removed(&ifc->net_port_proto_);
    }
  }

  mlan_status HandleConnectScan(pmlan_ioctl_req req) {
    auto scan = reinterpret_cast<mlan_ds_scan*>(req->pbuf);
    if (scan->sub_command != MLAN_OID_SCAN_USER_CONFIG) {
      // This wasn't a scan request, fail it so the tests can be updated to handle it. If the event
      // is triggered syncrhonously the tests will deadlock.
      return MLAN_STATUS_FAILURE;
    }
    // Asynchronously post an event about scan results.
    async::PostTask(dispatcher_loop_->dispatcher(), [this]() {
      mlan_event event{.event_id = MLAN_EVENT_ID_DRV_SCAN_REPORT};
      event_handler_.OnEvent(&event);
    });
    return MLAN_STATUS_SUCCESS;
  }

  // DataPlaneIfc implementation
  void OnEapolTransmitted(wlan::drivers::components::Frame&& frame, zx_status_t status) override {
    sync_completion_signal(&on_eapol_transmitted_called_);
  }
  void OnEapolReceived(wlan::drivers::components::Frame&& frame) override {
    sync_completion_signal(&on_eapol_received_called_);
  }

  void StartInterface() {
    auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
    EXPECT_FALSE(endpoints.is_error());
    fdf::BindServer(server_dispatcher_.get(), std::move(endpoints->server), this);

    auto result = client_.buffer(test_arena_)->Start(std::move(endpoints->client));
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_error());

    out_mlme_channel_ = result->value()->sme_channel.get();
  }

  // The Storage structure for the results of WlanFullmacIfc protocol.
  struct WlanFullmacIfcResultStorage {
    fuchsia_wlan_fullmac::wire::WlanFullmacScanEnd scan_end_;
    bool scan_end_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacConnectConfirm connect_conf_;
    bool connect_conf_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacAuthInd auth_ind_;
    bool auth_ind_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacDeauthConfirm deauth_conf_;
    // Use completion as the invoke flag when the event could be triggered by a asynchronous
    // routine to avoid flakeness.
    libsync::Completion deauth_conf_called_;
    fuchsia_wlan_fullmac::wire::WlanFullmacDeauthIndication deauth_ind_;
    bool deauth_ind_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacAssocInd assoc_ind_;
    bool assoc_ind_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacDisassocIndication disassoc_ind_;
    bool disassoc_ind_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacStartConfirm start_conf_;
    bool start_conf_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacStopConfirm stop_conf_;
    bool stop_conf_called_ = false;
    fuchsia_wlan_fullmac::wire::WlanFullmacSignalReportIndication report_ind_;
    bool report_ind_called_ = false;
  };

  // Implementation of fuchsia_wlan_fullmac::WlanFullmacImplIfc.
  void OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                 OnScanEndCompleter::Sync& completer) override {
    ifc_results_.scan_end_.txn_id = request->end.txn_id;
    ifc_results_.scan_end_called_ = true;
    completer.buffer(arena).Reply();
  }

  void ConnectConf(ConnectConfRequestView request, fdf::Arena& arena,
                   ConnectConfCompleter::Sync& completer) override {
    ifc_results_.connect_conf_called_ = true;
    completer.buffer(arena).Reply();
  }

  void AuthInd(AuthIndRequestView request, fdf::Arena& arena,
               AuthIndCompleter::Sync& completer) override {
    memcpy(ifc_results_.auth_ind_.peer_sta_address.data(), request->resp.peer_sta_address.data(),
           ETH_ALEN);
    ifc_results_.auth_ind_.auth_type = request->resp.auth_type;
    ifc_results_.auth_ind_called_ = true;
    completer.buffer(arena).Reply();
  }

  void DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                  DeauthConfCompleter::Sync& completer) override {
    memcpy(ifc_results_.deauth_conf_.peer_sta_address.data(), request->resp.peer_sta_address.data(),
           ETH_ALEN);
    ifc_results_.deauth_conf_called_.Signal();
    completer.buffer(arena).Reply();
  }

  void DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                 DeauthIndCompleter::Sync& completer) override {
    memcpy(ifc_results_.deauth_ind_.peer_sta_address.data(), request->ind.peer_sta_address.data(),
           ETH_ALEN);
    ifc_results_.deauth_ind_called_ = true;
    completer.buffer(arena).Reply();
  }

  void AssocInd(AssocIndRequestView request, fdf::Arena& arena,
                AssocIndCompleter::Sync& completer) override {
    memcpy(ifc_results_.assoc_ind_.peer_sta_address.data(), request->resp.peer_sta_address.data(),
           ETH_ALEN);
    ifc_results_.assoc_ind_.listen_interval = request->resp.listen_interval;
    ifc_results_.assoc_ind_called_ = true;
    completer.buffer(arena).Reply();
  }

  void DisassocInd(DisassocIndRequestView request, fdf::Arena& arena,
                   DisassocIndCompleter::Sync& completer) override {
    memcpy(ifc_results_.disassoc_ind_.peer_sta_address.data(), request->ind.peer_sta_address.data(),
           ETH_ALEN);
    ifc_results_.disassoc_ind_called_ = true;
    completer.buffer(arena).Reply();
  }

  void StartConf(StartConfRequestView request, fdf::Arena& arena,
                 StartConfCompleter::Sync& completer) override {
    ifc_results_.start_conf_.result_code = request->resp.result_code;
    ifc_results_.start_conf_called_ = true;
    completer.buffer(arena).Reply();
  }

  void StopConf(StopConfRequestView request, fdf::Arena& arena,
                StopConfCompleter::Sync& completer) override {
    ifc_results_.stop_conf_.result_code = request->resp.result_code;
    ifc_results_.stop_conf_called_ = true;
    completer.buffer(arena).Reply();
  }

  void SignalReport(SignalReportRequestView request, fdf::Arena& arena,
                    SignalReportCompleter::Sync& completer) override {
    ifc_results_.report_ind_.rssi_dbm = request->ind.rssi_dbm;
    ifc_results_.report_ind_.snr_db = request->ind.snr_db;
    ifc_results_.report_ind_called_ = true;
    completer.buffer(arena).Reply();
  }

  // Handler functions that will not be invoked in this test.
  void OnScanResult(OnScanResultRequestView request, fdf::Arena& arena,
                    OnScanResultCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void RoamConf(RoamConfRequestView request, fdf::Arena& arena,
                RoamConfCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void DisassocConf(DisassocConfRequestView request, fdf::Arena& arena,
                    DisassocConfCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void EapolConf(EapolConfRequestView request, fdf::Arena& arena,
                 EapolConfCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                       OnChannelSwitchCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void EapolInd(EapolIndRequestView request, fdf::Arena& arena,
                EapolIndCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void RelayCapturedFrame(RelayCapturedFrameRequestView request, fdf::Arena& arena,
                          RelayCapturedFrameCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void OnPmkAvailable(OnPmkAvailableRequestView request, fdf::Arena& arena,
                      OnPmkAvailableCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void SaeHandshakeInd(SaeHandshakeIndRequestView request, fdf::Arena& arena,
                       SaeHandshakeIndCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void SaeFrameRx(SaeFrameRxRequestView request, fdf::Arena& arena,
                  SaeFrameRxCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }
  void OnWmmStatusResp(OnWmmStatusRespRequestView request, fdf::Arena& arena,
                       OnWmmStatusRespCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }

  WlanFullmacIfcResultStorage ifc_results_;

  network_device_ifc_protocol_ops_t netdev_ifc_proto_ops_{.add_port = &OnAddPort,
                                                          .remove_port = &OnRemovePort};
  network_device_ifc_protocol_t netdev_ifc_proto_{.ops = &netdev_ifc_proto_ops_, .ctx = this};

  network_port_protocol_t net_port_proto_;
  sync_completion_t on_add_port_called_;
  sync_completion_t on_remove_port_called_;
  sync_completion_t on_eapol_transmitted_called_;
  sync_completion_t on_eapol_received_called_;

  wlan::simulation::Environment env_;
  std::unique_ptr<async::Loop> dispatcher_loop_ = {};
  TestDevice* test_device_ = nullptr;
  wlan::nxpfmac::EventHandler event_handler_;
  wlan::nxpfmac::MockBus mock_bus_;
  wlan::nxpfmac::MlanMockAdapter mlan_mocks_;
  std::unique_ptr<wlan::nxpfmac::IoctlAdapter> ioctl_adapter_;
  std::unique_ptr<wlan::nxpfmac::TestDataPlane> test_data_plane_;
  DeviceContext context_;
  // This data member MUST BE LAST, because it needs to be destroyed first, ensuring that whatever
  // interface lifetimes are managed by it are destroyed before other data members.
  std::shared_ptr<MockDevice> parent_;

  // Driver dispatcher that manages the lifecycle of interface device, and carries it's outgoing
  // directory operations.
  fdf::Dispatcher driver_dispatcher_;
  libsync::Completion driver_completion_;

  // This test class servers as the server end of WlanPhyImplIfc protocol, this dispatcher binds to
  // this class to dispatcher FIDL requests.
  fdf::Dispatcher server_dispatcher_;
  libsync::Completion server_completion_;

  // The FIDL client used to communicate with the interface device which is created in this test.
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl> client_;

  // The arena that backs the FIDL messages from this test to the interface device.
  fdf::Arena test_arena_;

  // The channel read out from the interface when starting it.
  zx_handle_t out_mlme_channel_;
};

TEST_F(WlanInterfaceTest, Construct) {
  // Test that an interface can be constructed and that its lifetime is managed correctly by the
  // mock device parent. It will call release on the interface and destroy it that way.

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, zx::channel()));
}

TEST_F(WlanInterfaceTest, WlanFullmacImplStartClient) {
  // Test that WlanFullmacImplStart works and correctly passes the MLME channel back.

  zx::channel in_client_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_client_mlme_channel, &unused));

  const zx_handle_t client_mlme_channel_handle = in_client_mlme_channel.get();

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, std::move(in_client_mlme_channel)));

  StartInterface();

  // Verify that the channel we get back from starting is the same we passed in during construction.
  // The one passed in during construction will be the one passed through wlanphy and we have to
  // pass it back here.
  ASSERT_EQ(client_mlme_channel_handle, out_mlme_channel_);
}

TEST_F(WlanInterfaceTest, WlanFullmacImplStartSoftAp) {
  zx::channel in_softap_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_softap_mlme_channel, &unused));

  const zx_handle_t softap_mlme_channel_handle = in_softap_mlme_channel.get();

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacSoftApDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kAp, &context_,
                                  kSoftApMacAddress, std::move(in_softap_mlme_channel)));

  StartInterface();

  // Verify that the channel we get back from starting is the same we passed in during construction.
  // The one passed in during construction will be the one passed through wlanphy and we have to
  // pass it back here.
  ASSERT_EQ(softap_mlme_channel_handle, out_mlme_channel_);
}

TEST_F(WlanInterfaceTest, WlanFullmacImplQuery) {
  // Test that WlanFullmacImplQuery returns some reasonable values

  constexpr uint8_t kChannels[] = {1,   2,   3,   4,   5,   6,   7,   8,   9,   10,  11,  12,  13,
                                   36,  40,  44,  48,  52,  56,  60,  64,  100, 104, 108, 112, 116,
                                   120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165, 255};
  constexpr size_t kNum2gChannels = 13;
  constexpr size_t kNum5gChannels = 26;

  constexpr fuchsia_wlan_common::wire::WlanMacRole kRole =
      fuchsia_wlan_common::wire::WlanMacRole::kAp;
  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0, kRole, &context_,
                                  kSoftApMacAddress, zx::channel()));

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->req_id == MLAN_IOCTL_BSS && req->action == MLAN_ACT_GET) {
      // Get the supported channel list.
      auto bss = reinterpret_cast<mlan_ds_bss*>(req->pbuf);
      EXPECT_EQ(MLAN_OID_BSS_CHANNEL_LIST, bss->sub_command);
      chan_freq* chan = bss->param.chanlist.cf;
      for (auto channel : kChannels) {
        (chan++)->channel = channel;
      }
      bss->param.chanlist.num_of_chan = std::size(kChannels);
    }
    return MLAN_STATUS_SUCCESS;
  });

  auto result = client_.buffer(test_arena_)->Query();
  ASSERT_TRUE(result.ok());
  ASSERT_FALSE(result->is_error());
  auto& info = result->value()->info;

  // Should match the role we provided at construction
  ASSERT_EQ(kRole, info.role);

  // Should support both bands
  ASSERT_EQ(2u, info.band_cap_count);
  EXPECT_EQ(fuchsia_wlan_common::wire::WlanBand::kTwoGhz, info.band_cap_list.data()[0].band);
  EXPECT_EQ(fuchsia_wlan_common::wire::WlanBand::kFiveGhz, info.band_cap_list.data()[1].band);

  // Should support a non-zero number of rates and channels for 2.4 GHz
  EXPECT_NE(0, info.band_cap_list[0].basic_rate_count);
  EXPECT_EQ(kNum2gChannels, info.band_cap_list[0].operating_channel_count);
  EXPECT_BYTES_EQ(kChannels, info.band_cap_list[0].operating_channel_list.data(), kNum2gChannels);

  // Should support a non-zero number of rates and channels for 5 GHz
  EXPECT_NE(0, info.band_cap_list[1].basic_rate_count);
  EXPECT_EQ(kNum5gChannels, info.band_cap_list.data()[1].operating_channel_count);
  EXPECT_BYTES_EQ(kChannels + kNum2gChannels,
                  info.band_cap_list.data()[1].operating_channel_list.data(), kNum5gChannels);
}

TEST_F(WlanInterfaceTest, WlanFullmacImplQueryMacSublayerSupport) {
  // Test that the most important values are configured in the mac sublayer support.

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, zx::channel()));

  auto result = client_.buffer(test_arena_)->QueryMacSublayerSupport();
  ASSERT_TRUE(result.ok());
  ASSERT_FALSE(result->is_error());
  auto& sublayer_support = result->value()->resp;

  // Data plan must be network device
  ASSERT_EQ(fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice,
            sublayer_support.data_plane.data_plane_type);
  // This is a fullmac device.
  ASSERT_EQ(fuchsia_wlan_common::wire::MacImplementationType::kFullmac,
            sublayer_support.device.mac_implementation_type);
}

TEST_F(WlanInterfaceTest, WlanFullmacImplStartScan) {
  // Test that calling start scan will eventually issue a scan IOCTL, more detailed scan tests exist
  // in the dedicated scanner tests.

  constexpr uint64_t kScanTxnId = 0x34435457;

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_SCAN) {
      // Start scan, has to be completed asynchronously.
      ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
      return MLAN_STATUS_PENDING;
    }
    if (req->action == MLAN_ACT_GET && req->req_id == MLAN_IOCTL_SCAN) {
      // Get scan results
      auto scan_req = reinterpret_cast<mlan_ds_scan*>(req->pbuf);
      // Make it easy for ourselves by saying there are no scan results.
      scan_req->param.scan_resp.num_in_scan_table = 0;
      return MLAN_STATUS_SUCCESS;
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, std::move(in_mlme_channel)));

  StartInterface();

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest::Builder(test_arena_);
  builder.txn_id(kScanTxnId);
  builder.scan_type(fuchsia_wlan_fullmac::wire::WlanScanType::kActive);

  auto scan_request = builder.Build();

  auto result = client_.buffer(test_arena_)->StartScan(scan_request);
  ASSERT_TRUE(result.ok());

  mlan_event scan_report_event{.event_id = MLAN_EVENT_ID_DRV_SCAN_REPORT};

  // Send a report indicating there's a scan report.
  event_handler_.OnEvent(&scan_report_event);

  // Because there are no scan results we just expect a scan end. We really only need to verify that
  // the scanner was called as a result of our StartScan call.
  EXPECT_TRUE(ifc_results_.scan_end_called_);
  EXPECT_EQ(kScanTxnId, ifc_results_.scan_end_.txn_id);
}

TEST_F(WlanInterfaceTest, WlanFullmacImplConnectDisconnectReq) {
  // Test that a connect request results in a successful connection. Also verify that a signal
  // report indication is received after a successful connection. And verify the client
  // disconnects when a local disconnect request is injected. More detailed tests exist in
  // the dedicated ClientConnection tests.

  constexpr int8_t kTestRssi = -64;
  constexpr int8_t kTestSnr = 28;
  constexpr zx::duration kSignalLogTimeout = zx::sec(30);

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      // Connect request, must complete asynchronously.
      ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
      return MLAN_STATUS_PENDING;
    }
    if (req->action == MLAN_ACT_GET && req->req_id == MLAN_IOCTL_GET_INFO) {
      auto signal_info = reinterpret_cast<mlan_ds_get_info*>(req->pbuf);
      if (signal_info->sub_command == MLAN_OID_GET_SIGNAL) {
        // Get signal request, must return asynchronously.
        signal_info->param.signal.data_snr_avg = kTestSnr;
        signal_info->param.signal.data_rssi_avg = kTestRssi;
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
    }
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_SCAN) {
      // As part of connecting WlanInterface will perform a connect scan, make it succeed.
      return HandleConnectScan(req);
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, std::move(in_mlme_channel)));

  StartInterface();

  // Prepare and send the connect request.
  constexpr uint8_t kIesWithSsid[] = {"\x00\x04Test"};
  constexpr uint8_t kTestChannel = 1;

  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest::Builder(test_arena_);

  fuchsia_wlan_internal::wire::BssDescription bss = {
      .ies = fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kIesWithSsid),
                                                     sizeof(kIesWithSsid)),
      .channel = {.primary = kTestChannel},
  };

  builder.selected_bss(bss);
  builder.auth_type(fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem);
  auto connect_request = builder.Build();

  {
    auto result = client_.buffer(test_arena_)->ConnectReq(connect_request);
    ASSERT_TRUE(result.ok());
  }

  // Wait until the timer has been scheduled.
  while (1) {
    if (env_.GetLatestEventTime().get() >= kSignalLogTimeout.to_nsecs()) {
      break;
    }
  }
  // Let the timer run
  env_.Run(kSignalLogTimeout);

  // Check to see if connect confirmation and signal indication were received.
  EXPECT_TRUE(ifc_results_.connect_conf_called_);
  EXPECT_TRUE(ifc_results_.report_ind_called_);
  EXPECT_EQ(ifc_results_.report_ind_.rssi_dbm, kTestRssi);
  EXPECT_EQ(ifc_results_.report_ind_.snr_db, kTestSnr);

  constexpr uint8_t kTestPeerAddr[] = {1, 2, 3, 4, 5, 6};
  fuchsia_wlan_fullmac::wire::WlanFullmacDeauthReq deauth_req = {
      .reason_code = fuchsia_wlan_ieee80211::wire::ReasonCode::kReserved0,
  };
  memcpy(deauth_req.peer_sta_address.data(), kTestPeerAddr, sizeof(kTestPeerAddr));

  {
    auto result = client_.buffer(test_arena_)->DeauthReq(deauth_req);
    ASSERT_TRUE(result.ok());
  }

  ifc_results_.deauth_conf_called_.Wait();
}

TEST_F(WlanInterfaceTest, WlanFullmacImplConnectRemoteDisconnectReq) {
  // Test that a connect request results in a successful connection. Also verify that a signal
  // report indication is received after a successful connection. And verify the client
  // disconnects when a FW disconnect event is injected. More detailed tests exist in
  // the dedicated ClientConnection tests.

  constexpr int8_t kTestRssi = -64;
  constexpr int8_t kTestSnr = 28;
  constexpr uint8_t kBssIndex = 0;
  constexpr zx::duration kSignalLogTimeout = zx::sec(30);

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      // Connect request, must complete asynchronously.
      ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
      return MLAN_STATUS_PENDING;
    }
    if (req->action == MLAN_ACT_GET && req->req_id == MLAN_IOCTL_GET_INFO) {
      auto signal_info = reinterpret_cast<mlan_ds_get_info*>(req->pbuf);
      if (signal_info->sub_command == MLAN_OID_GET_SIGNAL) {
        // Get signal request, must return asynchronously.
        signal_info->param.signal.data_snr_avg = kTestSnr;
        signal_info->param.signal.data_rssi_avg = kTestRssi;
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      }
    }
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_SCAN) {
      // As part of connecting WlanInterface will perform a connect scan, make it succeed.
      return HandleConnectScan(req);
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, kBssIndex,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, std::move(in_mlme_channel)));
  StartInterface();

  constexpr uint8_t kIesWithSsid[] = {"\x00\x04Test"};
  constexpr uint8_t kTestChannel = 1;
  auto builder = fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest::Builder(test_arena_);

  fuchsia_wlan_internal::wire::BssDescription bss = {
      .ies = fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kIesWithSsid),
                                                     sizeof(kIesWithSsid)),
      .channel = {.primary = kTestChannel},
  };

  builder.selected_bss(bss);
  builder.auth_type(fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem);
  auto connect_request = builder.Build();

  auto result = client_.buffer(test_arena_)->ConnectReq(connect_request);
  ASSERT_TRUE(result.ok());

  // Wait until the timer has been scheduled.
  while (1) {
    if (env_.GetLatestEventTime().get() >= kSignalLogTimeout.to_nsecs()) {
      break;
    }
  }
  // Let the timer run
  env_.Run(kSignalLogTimeout);

  // Verify connect confirmation and signal indication were received.
  EXPECT_TRUE(ifc_results_.connect_conf_called_);
  EXPECT_TRUE(ifc_results_.report_ind_called_);
  EXPECT_EQ(ifc_results_.report_ind_.rssi_dbm, kTestRssi);
  EXPECT_EQ(ifc_results_.report_ind_.snr_db, kTestSnr);

  // Simulate a remote disconnect by injecting a FW DISCONNECTED event.
  constexpr uint16_t kDisconnectReasonCode = 1;
  uint8_t event_buf[sizeof(mlan_event) + ETH_ALEN + 2];
  auto event = reinterpret_cast<pmlan_event>(event_buf);
  event->event_id = MLAN_EVENT_ID_FW_DISCONNECTED;
  memcpy(event->event_buf, &kDisconnectReasonCode, sizeof(kDisconnectReasonCode));
  event->event_len = sizeof(kDisconnectReasonCode);
  event->bss_index = kBssIndex;
  event_handler_.OnEvent(event);

  // confirm that the deauth indication was received.
  EXPECT_TRUE(ifc_results_.deauth_ind_called_);
}

TEST_F(WlanInterfaceTest, MacSetMode) {
  // Test that MacSetMode actually sets the mac mode.
  constexpr uint8_t kMacMulticastFilters[] = {0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
                                              0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c};

  std::atomic<mode_t> mac_mode;

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->action == MLAN_ACT_SET && req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss*>(req->pbuf);
      if (bss->sub_command == MLAN_OID_BSS_MULTICAST_LIST) {
        switch (mac_mode.load()) {
          case MODE_MULTICAST_FILTER:
            EXPECT_EQ(MLAN_MULTICAST_MODE, bss->param.multicast_list.mode);
            EXPECT_EQ(sizeof(kMacMulticastFilters) / ETH_ALEN,
                      bss->param.multicast_list.num_multicast_addr);
            EXPECT_BYTES_EQ(kMacMulticastFilters, bss->param.multicast_list.mac_list,
                            sizeof(kMacMulticastFilters));
            break;
          case MODE_MULTICAST_PROMISCUOUS:
            EXPECT_EQ(MLAN_ALL_MULTI_MODE, bss->param.multicast_list.mode);
            EXPECT_EQ(0, bss->param.multicast_list.num_multicast_addr);
            break;
          case MODE_PROMISCUOUS:
            EXPECT_EQ(MLAN_PROMISC_MODE, bss->param.multicast_list.mode);
            EXPECT_EQ(0, bss->param.multicast_list.num_multicast_addr);
            break;
          default:
            ADD_FAILURE("Unexpected mac mode: %u", mac_mode.load());
            break;
        }
        return MLAN_STATUS_SUCCESS;
      }
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacClientDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kClient, &context_,
                                  kClientMacAddress, std::move(in_mlme_channel)));
  WlanInterface* ifc = GetInterface();

  constexpr mode_t kMacModes[] = {MODE_MULTICAST_FILTER, MODE_MULTICAST_PROMISCUOUS,
                                  MODE_PROMISCUOUS};
  for (auto mode : kMacModes) {
    mac_mode = mode;
    ifc->MacSetMode(mode, kMacMulticastFilters);
  }
}

TEST_F(WlanInterfaceTest, WlanFullmacImplStartReq) {
  // Test that a SoftAP Start request results in the right set of ioctls and parameters.

  constexpr uint8_t kSoftApSsid[] = {"Test_SoftAP"};
  constexpr uint8_t kTestChannel = 6;

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss*>(req->pbuf);
      if (bss->sub_command == MLAN_OID_UAP_BSS_CONFIG) {
        if (req->action == MLAN_ACT_SET) {
          // BSS config set. Ensure SSID, channel and Band are correctly set.
          EXPECT_EQ(bss->param.bss_config.ssid.ssid_len, sizeof(kSoftApSsid));
          EXPECT_BYTES_EQ(bss->param.bss_config.ssid.ssid, kSoftApSsid,
                          bss->param.bss_config.ssid.ssid_len);
          EXPECT_EQ(bss->param.bss_config.channel, kTestChannel);
          EXPECT_EQ(bss->param.bss_config.bandcfg.chanBand, BAND_2GHZ);
          EXPECT_EQ(bss->param.bss_config.bandcfg.chanWidth, CHAN_BW_20MHZ);
        }
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      } else if (bss->sub_command == MLAN_OID_BSS_START ||
                 bss->sub_command == MLAN_OID_UAP_BSS_RESET) {
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        if (bss->sub_command == MLAN_OID_BSS_START) {
          uint8_t event_buf[sizeof(mlan_event)];
          auto event = reinterpret_cast<pmlan_event>(event_buf);
          event->event_id = MLAN_EVENT_ID_UAP_FW_BSS_START;
          event->bss_index = 0;
          event->event_len = 0;
          event_handler_.OnEvent(event);
        }
        return MLAN_STATUS_PENDING;
      }
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacSoftApDeviceName, 0,
                                  fuchsia_wlan_common::wire::WlanMacRole::kAp, &context_,
                                  kSoftApMacAddress, std::move(in_mlme_channel)));
  StartInterface();

  // Start the SoftAP
  fuchsia_wlan_fullmac::wire::WlanFullmacStartReq start_req = {
      .bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure,
      .channel = kTestChannel,
  };
  memcpy(start_req.ssid.data.data(), kSoftApSsid, sizeof(kSoftApSsid));
  start_req.ssid.len = sizeof(kSoftApSsid);

  {
    auto result = client_.buffer(test_arena_)->StartReq(start_req);
    ASSERT_TRUE(result.ok());
  }

  EXPECT_TRUE(ifc_results_.start_conf_called_);
  EXPECT_EQ(ifc_results_.start_conf_.result_code,
            fuchsia_wlan_fullmac::wire::WlanStartResult::kSuccess);

  // And now ensure SoftAP Stop works ok.
  fuchsia_wlan_fullmac::wire::WlanFullmacStopReq stop_req;
  memcpy(stop_req.ssid.data.data(), kSoftApSsid, sizeof(kSoftApSsid));
  stop_req.ssid.len = sizeof(kSoftApSsid);

  {
    auto result = client_.buffer(test_arena_)->StopReq(stop_req);
    ASSERT_TRUE(result.ok());
  }

  EXPECT_TRUE(ifc_results_.stop_conf_called_);
  EXPECT_EQ(ifc_results_.stop_conf_.result_code,
            fuchsia_wlan_fullmac::wire::WlanStopResult::kSuccess);
}

// Check to see SoftAP auth and assoc indications are received when a STA connects to it.
// Also verify deauth and disassoc indications are received when a STA disconnects from it.
TEST_F(WlanInterfaceTest, SoftApStaConnectDisconnect) {
  // Test that STA connect, disconnect indications are received correctly.

  constexpr uint8_t kTestSoftApClient[] = {0x0, 0x1, 0x2, 0x3, 0x4, 0x5};
  constexpr uint8_t kSoftApSsid[] = {"Test_SoftAP"};
  constexpr uint8_t kTestChannel = 6;
  constexpr uint8_t kBssIndex = 1;

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss*>(req->pbuf);
      if (bss->sub_command == MLAN_OID_UAP_BSS_CONFIG) {
        if (req->action == MLAN_ACT_SET) {
          // BSS config set. Ensure SSID, channel and Band are correctly set.
          EXPECT_EQ(bss->param.bss_config.ssid.ssid_len, sizeof(kSoftApSsid));
          EXPECT_BYTES_EQ(bss->param.bss_config.ssid.ssid, kSoftApSsid,
                          bss->param.bss_config.ssid.ssid_len);
          EXPECT_EQ(bss->param.bss_config.channel, kTestChannel);
          EXPECT_EQ(bss->param.bss_config.bandcfg.chanBand, BAND_2GHZ);
          EXPECT_EQ(bss->param.bss_config.bandcfg.chanWidth, CHAN_BW_20MHZ);
        }
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      } else if (bss->sub_command == MLAN_OID_BSS_START ||
                 bss->sub_command == MLAN_OID_UAP_BSS_RESET) {
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        if (bss->sub_command == MLAN_OID_BSS_START) {
          uint8_t event_buf[sizeof(mlan_event)];
          auto event = reinterpret_cast<pmlan_event>(event_buf);
          event->event_id = MLAN_EVENT_ID_UAP_FW_BSS_START;
          event->bss_index = kBssIndex;
          event->event_len = 0;
          event_handler_.OnEvent(event);
        }
        return MLAN_STATUS_PENDING;
      }
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacSoftApDeviceName, kBssIndex,
                                  fuchsia_wlan_common::wire::WlanMacRole::kAp, &context_,
                                  kSoftApMacAddress, std::move(in_mlme_channel)));

  StartInterface();

  // Start the SoftAP
  fuchsia_wlan_fullmac::wire::WlanFullmacStartReq start_req = {
      .bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure,
      .channel = kTestChannel,
  };
  memcpy(start_req.ssid.data.data(), kSoftApSsid, sizeof(kSoftApSsid));
  start_req.ssid.len = sizeof(kSoftApSsid);

  {
    auto result = client_.buffer(test_arena_)->StartReq(start_req);
    ASSERT_TRUE(result.ok());
  }

  EXPECT_TRUE(ifc_results_.start_conf_called_);
  EXPECT_EQ(ifc_results_.start_conf_.result_code,
            fuchsia_wlan_fullmac::wire::WlanStartResult::kSuccess);

  // Send a STA connect event
  uint8_t event_buf[sizeof(mlan_event) + ETH_ALEN + 2];
  auto event = reinterpret_cast<pmlan_event>(event_buf);
  event->event_id = MLAN_EVENT_ID_UAP_FW_STA_CONNECT;
  memcpy(event->event_buf, kTestSoftApClient, sizeof(kTestSoftApClient));
  event->event_len = sizeof(kTestSoftApClient);
  event->bss_index = kBssIndex;
  event_handler_.OnEvent(event);

  // Wait for auth and assoc indications.
  EXPECT_TRUE(ifc_results_.auth_ind_called_);
  EXPECT_BYTES_EQ(ifc_results_.auth_ind_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);
  EXPECT_EQ(ifc_results_.auth_ind_.auth_type,
            fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem);

  EXPECT_TRUE(ifc_results_.assoc_ind_called_);
  EXPECT_BYTES_EQ(ifc_results_.assoc_ind_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);
  EXPECT_EQ(ifc_results_.assoc_ind_.listen_interval, 0);

  // Followed by the STA disconnect event.
  event->event_id = MLAN_EVENT_ID_UAP_FW_STA_DISCONNECT;
  memcpy(event->event_buf + 2, kTestSoftApClient, sizeof(kTestSoftApClient));
  event->event_len = sizeof(kTestSoftApClient) + 2;
  event->bss_index = kBssIndex;
  event_handler_.OnEvent(event);

  // Wait for deauth and disassoc indications.
  EXPECT_TRUE(ifc_results_.deauth_ind_called_);
  EXPECT_BYTES_EQ(ifc_results_.deauth_ind_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);

  EXPECT_TRUE(ifc_results_.disassoc_ind_called_);
  EXPECT_BYTES_EQ(ifc_results_.disassoc_ind_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);

  // And now ensure SoftAP Stop works ok.
  fuchsia_wlan_fullmac::wire::WlanFullmacStopReq stop_req = {};
  memcpy(stop_req.ssid.data.data(), kSoftApSsid, sizeof(kSoftApSsid));
  stop_req.ssid.len = sizeof(kSoftApSsid);

  {
    auto result = client_.buffer(test_arena_)->StopReq(stop_req);
    ASSERT_TRUE(result.ok());
  }

  EXPECT_TRUE(ifc_results_.stop_conf_called_);
  EXPECT_EQ(ifc_results_.stop_conf_.result_code,
            fuchsia_wlan_fullmac::wire::WlanStopResult::kSuccess);
}

// Check to see SoftAP auth and assoc indications are received when a STA connects to it.
// Also verify a connected STA can be deauth'd by the SoftAP explicitly.
TEST_F(WlanInterfaceTest, SoftApStaLocalDisconnect) {
  // Test that STA connect, disconnect indications are received correctly.

  constexpr uint8_t kTestSoftApClient[] = {0x0, 0x1, 0x2, 0x3, 0x4, 0x5};
  constexpr uint8_t kSoftApSsid[] = {"Test_SoftAP"};
  constexpr uint8_t kTestChannel = 6;
  constexpr uint8_t kBssIndex = 1;

  mlan_mocks_.SetOnMlanIoctl([&](t_void*, pmlan_ioctl_req req) -> mlan_status {
    if (req->req_id == MLAN_IOCTL_BSS) {
      auto bss = reinterpret_cast<const mlan_ds_bss*>(req->pbuf);
      if (bss->sub_command == MLAN_OID_UAP_BSS_CONFIG) {
        if (req->action == MLAN_ACT_SET) {
          // BSS config set. Ensure SSID, channel and Band are correctly set.
          EXPECT_EQ(bss->param.bss_config.ssid.ssid_len, sizeof(kSoftApSsid));
          EXPECT_BYTES_EQ(bss->param.bss_config.ssid.ssid, kSoftApSsid,
                          bss->param.bss_config.ssid.ssid_len);
          EXPECT_EQ(bss->param.bss_config.channel, kTestChannel);
          EXPECT_EQ(bss->param.bss_config.bandcfg.chanBand, BAND_2GHZ);
          EXPECT_EQ(bss->param.bss_config.bandcfg.chanWidth, CHAN_BW_20MHZ);
        }
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        return MLAN_STATUS_PENDING;
      } else if (bss->sub_command == MLAN_OID_BSS_START ||
                 bss->sub_command == MLAN_OID_UAP_BSS_RESET) {
        ioctl_adapter_->OnIoctlComplete(req, wlan::nxpfmac::IoctlStatus::Success);
        if (bss->sub_command == MLAN_OID_BSS_START) {
          uint8_t event_buf[sizeof(mlan_event)];
          auto event = reinterpret_cast<pmlan_event>(event_buf);
          event->event_id = MLAN_EVENT_ID_UAP_FW_BSS_START;
          event->bss_index = kBssIndex;
          event->event_len = 0;
          event_handler_.OnEvent(event);
        }
        return MLAN_STATUS_PENDING;
      }
    }
    // Return success for everything else.
    return MLAN_STATUS_SUCCESS;
  });

  zx::channel in_mlme_channel;
  zx::channel unused;
  ASSERT_OK(zx::channel::create(0, &in_mlme_channel, &unused));

  ASSERT_OK(CreateDeviceInterface(parent_.get(), kFullmacSoftApDeviceName, kBssIndex,
                                  fuchsia_wlan_common::wire::WlanMacRole::kAp, &context_,
                                  kSoftApMacAddress, std::move(in_mlme_channel)));

  StartInterface();

  // Start the SoftAP
  fuchsia_wlan_fullmac::wire::WlanFullmacStartReq start_req = {
      .bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure,
      .channel = kTestChannel,
  };
  memcpy(start_req.ssid.data.data(), kSoftApSsid, sizeof(kSoftApSsid));
  start_req.ssid.len = sizeof(kSoftApSsid);

  {
    auto result = client_.buffer(test_arena_)->StartReq(start_req);
    ASSERT_TRUE(result.ok());
  }

  EXPECT_TRUE(ifc_results_.start_conf_called_);
  EXPECT_EQ(ifc_results_.start_conf_.result_code,
            fuchsia_wlan_fullmac::wire::WlanStartResult::kSuccess);

  // Send a STA connect event
  uint8_t event_buf[sizeof(mlan_event) + ETH_ALEN + 2];
  auto event = reinterpret_cast<pmlan_event>(event_buf);
  event->event_id = MLAN_EVENT_ID_UAP_FW_STA_CONNECT;
  memcpy(event->event_buf, kTestSoftApClient, sizeof(kTestSoftApClient));
  event->event_len = sizeof(kTestSoftApClient);
  event->bss_index = kBssIndex;
  event_handler_.OnEvent(event);

  // Wait for auth and assoc indications.
  EXPECT_TRUE(ifc_results_.auth_ind_called_);
  EXPECT_BYTES_EQ(ifc_results_.auth_ind_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);
  EXPECT_EQ(ifc_results_.auth_ind_.auth_type,
            fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem);

  EXPECT_TRUE(ifc_results_.assoc_ind_called_);
  EXPECT_BYTES_EQ(ifc_results_.assoc_ind_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);
  EXPECT_EQ(ifc_results_.assoc_ind_.listen_interval, 0);

  // Send a deauth request to disconnect the STA
  fuchsia_wlan_fullmac::wire::WlanFullmacDeauthReq deauth_req = {
      .reason_code = fuchsia_wlan_ieee80211::wire::ReasonCode::kReserved0,
  };
  memcpy(deauth_req.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);

  env_.ScheduleNotification(
      [&]() { EXPECT_TRUE(client_.buffer(test_arena_)->DeauthReq(deauth_req).ok()); },
      zx::msec(10));
  // We currently do not have a way to send the deauth event at the appropriate time. So let the
  // deauth request timeout waiting for the event and send a deauth conf.

  env_.Run(zx::sec(2));

  ifc_results_.deauth_conf_called_.Wait();
  EXPECT_BYTES_EQ(ifc_results_.deauth_conf_.peer_sta_address.data(), kTestSoftApClient, ETH_ALEN);

  // And now ensure SoftAP Stop works ok.
  fuchsia_wlan_fullmac::wire::WlanFullmacStopReq stop_req;
  memcpy(stop_req.ssid.data.data(), kSoftApSsid, sizeof(kSoftApSsid));
  stop_req.ssid.len = sizeof(kSoftApSsid);

  {
    auto result = client_.buffer(test_arena_)->StopReq(stop_req);
    ASSERT_TRUE(result.ok());
  }

  EXPECT_TRUE(ifc_results_.stop_conf_called_);
  EXPECT_EQ(ifc_results_.stop_conf_.result_code,
            fuchsia_wlan_fullmac::wire::WlanStopResult::kSuccess);
}

}  // namespace
