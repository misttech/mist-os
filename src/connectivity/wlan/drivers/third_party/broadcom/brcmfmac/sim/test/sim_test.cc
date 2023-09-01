// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/directory.h>

#include <fbl/string_buffer.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/factory_device.h"

namespace wlan::brcmfmac {

// static
const std::vector<uint8_t> SimInterface::kDefaultScanChannels = {
    1,  2,   3,   4,   5,   6,   7,   8,   9,   10,  11,  32,  36,  40,  44,  48,  52,  56, 60,
    64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165};

SimInterface::SimInterface() : test_arena_(fdf::Arena('IFAC')) {}

SimInterface::~SimInterface() {
  // If the client is valid, it means that this SimInterface has been connected to a real
  // WlanInterface object, otherwise skip the stop.
  if (client_.is_valid()) {
    auto result = client_.buffer(test_arena_)->Stop();
    ZX_ASSERT(result.ok());
  }
  if (ch_sme_ != ZX_HANDLE_INVALID) {
    zx_handle_close(ch_sme_);
  }
  if (ch_mlme_ != ZX_HANDLE_INVALID) {
    zx_handle_close(ch_mlme_);
  }

  // If this SimInterface has a role, Init() must be called, so there was a server_dispatcher_
  // created.
  DestroyDispatcher();
}

zx_status_t SimInterface::Init(std::shared_ptr<simulation::Environment> env,
                               wlan_common::WlanMacRole role) {
  zx_status_t result = zx_channel_create(0, &ch_sme_, &ch_mlme_);
  if (result == ZX_OK) {
    env_ = env;
    role_ = role;
  }
  return result;
}

void SimInterface::CreateDispatcher() {
  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "wlan_fullmac_ifc_test_server",
      [&](fdf_dispatcher_t*) { server_completion_.Signal(); });

  if (dispatcher.is_error()) {
    BRCMF_ERR("Creating server dispatcher error : %s\n", dispatcher.status_string());
  }

  server_dispatcher_ = std::move(*dispatcher);
  server_completion_.Reset();
}

zx_status_t SimInterface::Connect(
    fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> client_end) {
  client_ = fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl>(std::move(client_end));

  // Establish the FIDL connection on the oppsite direction.
  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
  if (endpoints.is_error()) {
    BRCMF_ERR("Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  fdf::BindServer(server_dispatcher_.get(), std::move(endpoints->server), this);

  auto result = client_.buffer(test_arena_)->Start(std::move(endpoints->client));
  if (!result.ok()) {
    BRCMF_ERR("Start failed, FIDL error: %s", result.status_string());
    return result.status();
  }

  if (result->is_error()) {
    BRCMF_ERR("Start failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Verify that the channel passed back from start() is the same one we gave to create_iface()
  if (result->value()->sme_channel.get() != ch_mlme_) {
    BRCMF_ERR("Channels don't match, sme_channel: %zu, ch_mlme_: %zu",
              result->value()->sme_channel.get(), ch_mlme_);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

void SimInterface::DestroyDispatcher() {
  if (server_dispatcher_.get()) {
    server_dispatcher_.ShutdownAsync();
    server_completion_.Wait();
    server_dispatcher_.release();
  }
}

void SimInterface::OnScanResult(OnScanResultRequestView request, fdf::Arena& arena,
                                OnScanResultCompleter::Sync& completer) {
  auto& copy = request->result;

  auto results = scan_results_.find(copy.txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  // Verify that the scan hasn't sent a completion notice
  ZX_ASSERT(!results->second.result_code);

  // Copy the IES data over since the original location may change data by the time we verify.
  std::vector<uint8_t> ies(copy.bss.ies.data(), copy.bss.ies.data() + copy.bss.ies.count());
  scan_results_ies_.push_back(ies);
  copy.bss.ies = fidl::VectorView<uint8_t>::FromExternal(*(scan_results_ies_.rbegin()));
  results->second.result_list.push_back(copy);
  completer.buffer(arena).Reply();
}

void SimInterface::OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                             OnScanEndCompleter::Sync& completer) {
  auto& end = request->end;
  auto results = scan_results_.find(end.txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  // Verify that the scan hasn't already received a completion notice
  ZX_ASSERT(!results->second.result_code);

  results->second.result_code = end.code;
  completer.buffer(arena).Reply();
}

void SimInterface::ConnectConf(ConnectConfRequestView request, fdf::Arena& arena,
                               ConnectConfCompleter::Sync& completer) {
  ZX_ASSERT(assoc_ctx_.state == AssocContext::kAssociating);
  auto& resp = request->resp;
  stats_.connect_results.push_back(resp);

  if (resp.result_code == wlan_ieee80211::StatusCode::kSuccess) {
    assoc_ctx_.state = AssocContext::kAssociated;
    stats_.connect_successes++;
  } else {
    assoc_ctx_.state = AssocContext::kNone;
  }
  completer.buffer(arena).Reply();
}

void SimInterface::RoamConf(RoamConfRequestView request, fdf::Arena& arena,
                            RoamConfCompleter::Sync& completer) {
  auto& resp = request->resp;
  ZX_ASSERT(assoc_ctx_.state == AssocContext::kAssociated);

  if (resp.result_code == wlan_ieee80211::StatusCode::kSuccess) {
    std::memcpy(assoc_ctx_.bssid.byte, resp.target_bssid.data(), ETH_ALEN);
    stats_.connect_successes++;
  } else {
    assoc_ctx_.state = AssocContext::kNone;
  }
  completer.buffer(arena).Reply();
}

void SimInterface::AuthInd(AuthIndRequestView request, fdf::Arena& arena,
                           AuthIndCompleter::Sync& completer) {
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);
  stats_.auth_indications.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                              DeauthConfCompleter::Sync& completer) {
  stats_.deauth_results.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                             DeauthIndCompleter::Sync& completer) {
  stats_.deauth_indications.push_back(request->ind);
  completer.buffer(arena).Reply();
}

void SimInterface::AssocInd(AssocIndRequestView request, fdf::Arena& arena,
                            AssocIndCompleter::Sync& completer) {
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);
  stats_.assoc_indications.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::DisassocConf(DisassocConfRequestView request, fdf::Arena& arena,
                                DisassocConfCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::DisassocInd(DisassocIndRequestView request, fdf::Arena& arena,
                               DisassocIndCompleter::Sync& completer) {
  stats_.disassoc_indications.push_back(request->ind);
  completer.buffer(arena).Reply();
}

void SimInterface::StartConf(StartConfRequestView request, fdf::Arena& arena,
                             StartConfCompleter::Sync& completer) {
  stats_.start_confirmations.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::StopConf(StopConfRequestView request, fdf::Arena& arena,
                            StopConfCompleter::Sync& completer) {
  stats_.stop_confirmations.push_back(request->resp);
  completer.buffer(arena).Reply();
}

void SimInterface::EapolConf(EapolConfRequestView request, fdf::Arena& arena,
                             EapolConfCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                                   OnChannelSwitchCompleter::Sync& completer) {
  stats_.csa_indications.push_back(request->ind);
  completer.buffer(arena).Reply();
}

void SimInterface::SignalReport(SignalReportRequestView request, fdf::Arena& arena,
                                SignalReportCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::EapolInd(EapolIndRequestView request, fdf::Arena& arena,
                            EapolIndCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::RelayCapturedFrame(RelayCapturedFrameRequestView request, fdf::Arena& arena,
                                      RelayCapturedFrameCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::OnPmkAvailable(OnPmkAvailableRequestView request, fdf::Arena& arena,
                                  OnPmkAvailableCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::SaeHandshakeInd(SaeHandshakeIndRequestView request, fdf::Arena& arena,
                                   SaeHandshakeIndCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::SaeFrameRx(SaeFrameRxRequestView request, fdf::Arena& arena,
                              SaeFrameRxCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::OnWmmStatusResp(OnWmmStatusRespRequestView request, fdf::Arena& arena,
                                   OnWmmStatusRespCompleter::Sync& completer) {
  completer.buffer(arena).Reply();
}

void SimInterface::StopInterface() {
  auto result = client_.buffer(test_arena_)->Stop();
  if (!result.ok()) {
    BRCMF_ERR("Stop failed, FIDL error: %s", result.status_string());
  }
}

void SimInterface::Query(wlan_fullmac::WlanFullmacQueryInfo* out_info) {
  auto result = client_.buffer(test_arena_)->Query();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_info = result->value()->info;
}

void SimInterface::QueryMacSublayerSupport(wlan_common::MacSublayerSupport* out_resp) {
  auto result = client_.buffer(test_arena_)->QueryMacSublayerSupport();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_resp = result->value()->resp;
}

void SimInterface::QuerySecuritySupport(wlan_common::SecuritySupport* out_resp) {
  auto result = client_.buffer(test_arena_)->QuerySecuritySupport();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_resp = result->value()->resp;
}

void SimInterface::QuerySpectrumManagementSupport(
    wlan_common::SpectrumManagementSupport* out_resp) {
  auto result = client_.buffer(test_arena_)->QuerySpectrumManagementSupport();
  ZX_ASSERT(result.ok());
  ZX_ASSERT(!result->is_error());

  *out_resp = result->value()->resp;
}

void SimInterface::GetMacAddr(common::MacAddr* out_macaddr) {
  wlan_fullmac::WlanFullmacQueryInfo info;
  Query(&info);
  memcpy(out_macaddr->byte, info.sta_addr.data(), ETH_ALEN);
}

void SimInterface::StartConnect(const common::MacAddr& bssid, const wlan_ieee80211::CSsid& ssid,
                                const wlan_common::WlanChannel& channel) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  stats_.connect_attempts++;

  // Save off context
  assoc_ctx_.state = AssocContext::kAssociating;
  assoc_ctx_.bssid = bssid;

  assoc_ctx_.ies.clear();
  assoc_ctx_.ies.push_back(0);         // SSID IE type ID
  assoc_ctx_.ies.push_back(ssid.len);  // SSID IE length
  assoc_ctx_.ies.insert(assoc_ctx_.ies.end(), ssid.data.data(), ssid.data.data() + ssid.len);
  assoc_ctx_.channel = channel;

  // Send connect request
  auto builder = wlan_fullmac::WlanFullmacImplConnectRequest::Builder(test_arena_);
  fuchsia_wlan_internal::wire::BssDescription bss;
  memcpy(bss.bssid.data(), bssid.byte, ETH_ALEN);
  auto ies =
      std::vector<uint8_t>(assoc_ctx_.ies.data(), assoc_ctx_.ies.data() + assoc_ctx_.ies.size());
  bss.ies = fidl::VectorView(test_arena_, ies);
  bss.channel = channel;
  bss.bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure;
  builder.selected_bss(bss);
  builder.auth_type(wlan_fullmac::WlanAuthType::kOpenSystem);
  builder.connect_failure_timeout(1000);  // ~1s (although value is ignored for now)

  auto result = client_.buffer(test_arena_)->Connect(builder.Build());
  ZX_ASSERT(result.ok());
}

void SimInterface::AssociateWith(const simulation::FakeAp& ap, std::optional<zx::duration> delay) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  common::MacAddr bssid = ap.GetBssid();
  wlan_ieee80211::CSsid ssid = ap.GetSsid();
  wlan_common::WlanChannel channel = ap.GetChannel();

  if (delay) {
    env_->ScheduleNotification(std::bind(&SimInterface::StartConnect, this, bssid, ssid, channel),
                               *delay);
  } else {
    StartConnect(ap.GetBssid(), ap.GetSsid(), ap.GetChannel());
  }
}

void SimInterface::DeauthenticateFrom(const common::MacAddr& bssid,
                                      wlan_ieee80211::ReasonCode reason) {
  // This should only be performed on a Client interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kClient);

  wlan_fullmac::WlanFullmacDeauthReq deauth_req = {.reason_code = reason};
  memcpy(deauth_req.peer_sta_address.data(), bssid.byte, ETH_ALEN);

  auto result = client_.buffer(test_arena_)->DeauthReq(deauth_req);
  ZX_ASSERT(result.ok());
}

void SimInterface::StartScan(uint64_t txn_id, bool active,
                             std::optional<const std::vector<uint8_t>> channels_arg) {
  wlan_fullmac::WlanScanType scan_type =
      active ? wlan_fullmac::WlanScanType::kActive : wlan_fullmac::WlanScanType::kPassive;
  uint32_t dwell_time = active ? kDefaultActiveScanDwellTimeMs : kDefaultPassiveScanDwellTimeMs;
  const std::vector<uint8_t> channels =
      channels_arg.has_value() ? channels_arg.value() : kDefaultScanChannels;

  auto builder = wlan_fullmac::WlanFullmacImplStartScanRequest::Builder(test_arena_);

  builder.txn_id(txn_id);
  builder.scan_type(scan_type);
  builder.channels(fidl::VectorView(test_arena_, channels));
  builder.min_channel_time(dwell_time);
  builder.max_channel_time(dwell_time);

  // Create an entry for tracking results
  ScanStatus scan_status;
  scan_results_.insert_or_assign(txn_id, scan_status);

  // Start the scan
  auto result = client_.buffer(test_arena_)->StartScan(builder.Build());
  ZX_ASSERT(result.ok());
}

std::optional<wlan_fullmac::WlanScanResult> SimInterface::ScanResultCode(uint64_t txn_id) {
  auto results = scan_results_.find(txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  return results->second.result_code;
}

const std::list<wlan_fullmac::WlanFullmacScanResult>* SimInterface::ScanResultList(
    uint64_t txn_id) {
  auto results = scan_results_.find(txn_id);

  // Verify that we started a scan on this interface
  ZX_ASSERT(results != scan_results_.end());

  return &results->second.result_list;
}

void SimInterface::StartSoftAp(const wlan_ieee80211::CSsid& ssid,
                               const wlan_common::WlanChannel& channel, uint32_t beacon_period,
                               uint32_t dtim_period) {
  // This should only be performed on an AP interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);

  wlan_fullmac::WlanFullmacStartReq start_req = {
      .bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure,
      .beacon_period = beacon_period,
      .dtim_period = dtim_period,
      .channel = channel.primary,
      .rsne_len = 0,
  };

  // Set the SSID field in the request
  start_req.ssid = ssid;

  // Send request to driver
  auto result = client_.buffer(test_arena_)->StartReq(start_req);
  ZX_ASSERT(result.ok());

  // Remember context
  soft_ap_ctx_.ssid = ssid;

  // Return value is handled asynchronously in OnStartConf
}

void SimInterface::StopSoftAp() {
  // This should only be performed on an AP interface
  ZX_ASSERT(role_ == wlan_common::WlanMacRole::kAp);

  wlan_fullmac::WlanFullmacStopReq stop_req;

  ZX_ASSERT(stop_req.ssid.data.size() == wlan_ieee80211::kMaxSsidByteLen);
  // Use the ssid from the last call to StartSoftAp
  stop_req.ssid = soft_ap_ctx_.ssid;

  // Send request to driver
  auto result = client_.buffer(test_arena_)->StopReq(stop_req);
  ZX_ASSERT(result.ok());
}

zx_status_t SimInterface::SetMulticastPromisc(bool enable) {
  auto result = client_.buffer(test_arena_)->SetMulticastPromisc(enable);
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
}

SimTest::SimTest() : test_arena_(fdf::Arena('T')), loop_(&kAsyncLoopConfigNeverAttachToThread) {
  env_ = std::make_shared<simulation::Environment>();
  env_->AddStation(this);

  dev_mgr_ = std::make_unique<simulation::FakeDevMgr>();
  // The sim test is strictly a theoretical observer in the simulation environment thus it should be
  // able to see everything
  rx_sensitivity_ = std::numeric_limits<double>::lowest();
  loop_.StartThread("factory-device-test");
}

SimTest::~SimTest() {
  // Clean the ifaces created in test but not deleted.
  for (auto iface : ifaces_) {
    auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_);
    builder.iface_id(iface.first);
    auto result = client_.sync().buffer(test_arena_)->DestroyIface(builder.Build());
    if (!result.ok()) {
      BRCMF_ERR("Delete iface: %u failed", iface.first);
    }
    if (result->is_error()) {
      BRCMF_ERR("Delete iface: %u failed", iface.first);
    }
  }

  libsync::Completion host_destroyed;
  async::PostTask(driver_dispatcher_.async_dispatcher(), [&]() {
    dev_mgr_.reset();
    host_destroyed.Signal();
  });
  host_destroyed.Wait();

  if (client_dispatcher_.get()) {
    client_dispatcher_.ShutdownAsync();
    client_completion_.Wait();
  }

  if (driver_dispatcher_.get()) {
    driver_dispatcher_.ShutdownAsync();
    completion_.Wait();
  }
  // Don't have to erase the iface ids here.
}

zx_status_t SimTest::PreInit() {
  // Create a dispatcher to wait on the runtime channel.
  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "sim-test",
      [&](fdf_dispatcher_t*) { completion_.Signal(); });

  if (dispatcher.is_error()) {
    BRCMF_ERR("Failed to create dispatcher : %s", zx_status_get_string(dispatcher.error_value()));
    return ZX_ERR_INTERNAL;
  }

  driver_dispatcher_ = *std::move(dispatcher);

  // Create the device on driver dispatcher because the outgoing directory is required to be
  // accessed by a single dispatcher.
  libsync::Completion created;
  async::PostTask(driver_dispatcher_.async_dispatcher(), [&]() {
    // Allocate memory for a simulated device and register with dev_mgr
    ASSERT_EQ(ZX_OK, brcmfmac::SimDevice::Create(dev_mgr_->GetRootDevice(), dev_mgr_.get(), env_,
                                                 &device_));
    created.Signal();
  });
  created.Wait();

  return ZX_OK;
}

zx_status_t SimTest::Init() {
  zx_status_t status;

  // Allocate device and register with dev_mgr
  if (device_ == nullptr) {
    status = PreInit();
    if (status != ZX_OK) {
      return status;
    }
  }

  // Initialize device
  status = device_->BusInit();
  if (status != ZX_OK) {
    // Ownership of the device has been transferred to the dev_mgr, so we don't need to dealloc it
    device_ = nullptr;
    return status;
  }

  // Create a dispatcher to wait on the runtime channel.
  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "sim-test",
      [&](fdf_dispatcher_t*) { client_completion_.Signal(); });

  if (dispatcher.is_error()) {
    BRCMF_ERR("Failed to create dispatcher : %s", zx_status_get_string(dispatcher.error_value()));
    return ZX_ERR_INTERNAL;
  }

  client_dispatcher_ = *std::move(dispatcher);

  auto outgoing_dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  EXPECT_FALSE(outgoing_dir_endpoints.is_error());

  // Serve WlanPhyImplProtocol to the device's outgoing directory on the driver dispatcher.
  libsync::Completion served;
  async::PostTask(driver_dispatcher_.async_dispatcher(), [&]() {
    ASSERT_EQ(ZX_OK, device_->ServeWlanPhyImplProtocol(std::move(outgoing_dir_endpoints->server)));
    served.Signal();
  });
  served.Wait();

  // Connect WlanPhyImpl protocol from this class, this operation mimics the implementation of
  // DdkConnectRuntimeProtocol().
  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_phyimpl::Service::WlanPhyImpl::ProtocolType>();
  EXPECT_FALSE(endpoints.is_error());
  zx::channel client_token, server_token;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &client_token, &server_token));
  EXPECT_EQ(ZX_OK, fdf::ProtocolConnect(std::move(client_token),
                                        fdf::Channel(endpoints->server.TakeChannel().release())));
  fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
  path.AppendPrintf("svc/%s/default/%s", fuchsia_wlan_phyimpl::Service::WlanPhyImpl::ServiceName,
                    fuchsia_wlan_phyimpl::Service::WlanPhyImpl::Name);
  // Serve the WlanPhyImpl protocol on `server_token` found at `path` within
  // the outgoing directory.
  EXPECT_EQ(ZX_OK, fdio_service_connect_at(outgoing_dir_endpoints->client.channel().get(),
                                           path.c_str(), server_token.release()));

  client_ = fdf::WireSharedClient<fuchsia_wlan_phyimpl::WlanPhyImpl>(std::move(endpoints->client),
                                                                     client_dispatcher_.get());

  device_->WaitForProtocolConnection();
  zx::result factory_endpoints = fidl::CreateEndpoints<fuchsia_factory_wlan::Iovar>();
  if (factory_endpoints.is_error()) {
    BRCMF_ERR("Failed to create factory dispatcher : %s",
              zx_status_get_string(factory_endpoints.error_value()));
    return ZX_ERR_INTERNAL;
  }
  auto [client_end, server_end] = std::move(*factory_endpoints);
  if (!client_end.is_valid()) {
    BRCMF_ERR("Failed to create client_end");
    return ZX_ERR_INTERNAL;
  }

  device_->Init();
  fidl::BindServer(loop_.dispatcher(), std::move(server_end), device_->GetFactoryDevice());

  factory_device_ = fidl::WireSyncClient(std::move(client_end));
  if (!factory_device_.is_valid()) {
    BRCMF_ERR("Failed to create device");
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

zx_status_t SimTest::StartInterface(wlan_common::WlanMacRole role, SimInterface* sim_ifc,
                                    std::optional<common::MacAddr> mac_addr) {
  zx_status_t status;
  if ((status = sim_ifc->Init(env_, role)) != ZX_OK) {
    return status;
  }
  auto ch = zx::channel(sim_ifc->ch_mlme_);

  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceRequest::Builder(test_arena_);
  builder.role(role);
  builder.mlme_channel(std::move(ch));
  if (mac_addr) {
    fidl::Array<unsigned char, 6> init_sta_addr;
    memcpy(&init_sta_addr, mac_addr.value().byte, ETH_ALEN);
    builder.init_sta_addr(init_sta_addr);
  }

  auto result = client_.sync().buffer(test_arena_)->CreateIface(builder.Build());

  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    BRCMF_ERR("%s error happened while creating interface",
              zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  sim_ifc->iface_id_ = result->value()->iface_id();
  sim_ifc->CreateDispatcher();

  status = ZX_OK;

  if (!ifaces_.insert_or_assign(sim_ifc->iface_id_, sim_ifc).second) {
    BRCMF_ERR("Iface already exist in this test.\n");
    return ZX_ERR_ALREADY_EXISTS;
  }

  // This should have created a WLAN_FULLMAC_IMPL device
  auto device = dev_mgr_->FindLatestByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL);
  if (device == nullptr) {
    return ZX_ERR_INTERNAL;
  }

  {
    auto endpoints =
        fdf::CreateEndpoints<fuchsia_wlan_fullmac::Service::WlanFullmacImpl::ProtocolType>();
    EXPECT_FALSE(endpoints.is_error());
    zx::channel client_token, server_token;
    status = zx::channel::create(0, &client_token, &server_token);
    if (status != ZX_OK) {
      BRCMF_ERR("Failed to create channel: %s", zx_status_get_string(status));
      return status;
    }
    status = fdf::ProtocolConnect(std::move(client_token),
                                  fdf::Channel(endpoints->server.TakeChannel().release()));
    if (status != ZX_OK) {
      BRCMF_ERR("ProtocolConnect Failed: %s", zx_status_get_string(status));
      return status;
    }
    fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
    path.AppendPrintf("svc/%s/default/%s",
                      fuchsia_wlan_fullmac::Service::WlanFullmacImpl::ServiceName,
                      fuchsia_wlan_fullmac::Service::WlanFullmacImpl::Name);
    // Serve the WlanFullmacImpl protocol on `server_token` found at `path` within
    // the outgoing directory. Here we get the client end outgoing_dir_channel from the device
    // managed by FakeDevMgr.
    status = fdio_service_connect_at(device->DevArgs().outgoing_dir_channel, path.c_str(),
                                     server_token.release());
    if (status != ZX_OK) {
      BRCMF_ERR("Failed to open the directory and connect to WlanFullmacImpl service: %s",
                zx_status_get_string(status));
      return status;
    }
    status = sim_ifc->Connect(std::move(endpoints->client));
    if (status != ZX_OK) {
      BRCMF_ERR("Failed to establish FIDL connection with WlanInterface: %s",
                zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t SimTest::InterfaceDestroyed(SimInterface* ifc) {
  auto iter = ifaces_.find(ifc->iface_id_);

  if (iter == ifaces_.end()) {
    BRCMF_ERR("Iface id: %d does not exist", ifc->iface_id_);
    return ZX_ERR_NOT_FOUND;
  }

  // Destroy the server_dispatcher_ so that when this SimInterface is started again, the
  // server_dispatcher_ can be overwritten.
  ifc->DestroyDispatcher();
  ifaces_.erase(iter);

  return ZX_OK;
}

zx_status_t SimTest::DeleteInterface(SimInterface* ifc) {
  auto iter = ifaces_.find(ifc->iface_id_);

  if (iter == ifaces_.end()) {
    BRCMF_ERR("Iface id: %d does not exist", ifc->iface_id_);
    return ZX_ERR_NOT_FOUND;
  }

  BRCMF_DBG(SIM, "Del IF: %d", ifc->iface_id_);

  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplDestroyIfaceRequest::Builder(test_arena_);
  builder.iface_id(iter->first);
  auto result = client_.sync().buffer(test_arena_)->DestroyIface(builder.Build());
  EXPECT_TRUE(result.ok());
  if (result->is_error()) {
    BRCMF_ERR("Failed to destroy interface.\n");
    return result->error_value();
  }

  // Destroy the server_dispatcher_ so that when this SimInterface is started again, the
  // server_dispatcher_ can be overwritten.
  ifc->DestroyDispatcher();
  // This operation destroyes the WireSyncClient talking to WlanInterface, and take it back to an
  // initialized state.
  ifc->client_.TakeClientEnd();
  // Once the interface data structures have been deleted, our pointers are no longer valid.
  ifaces_.erase(iter);

  return ZX_OK;
}

}  // namespace wlan::brcmfmac
