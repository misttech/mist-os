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

#include "device.h"

#include <lib/ddk/metadata.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/zx/timer.h>
#include <sys/random.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <ddktl/fidl.h>
#include <ddktl/init-txn.h>
#include <wlan/common/ieee80211.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/align.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/debug.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/moal_shim.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/moal_utils.h"

namespace wlan::nxpfmac {

constexpr char kClientInterfaceName[] = "nxpfmac-wlan-fullmac-client";
constexpr uint8_t kClientInterfaceId = 0;
constexpr char kApInterfaceName[] = "nxpfmac-wlan-fullmac-ap";
constexpr uint8_t kApInterfaceId = 1;

constexpr char kFirmwarePath[] = "nxpfmac/sdsd8987_combo.bin";
constexpr char kTxPwrWWPath[] = "nxpfmac/txpower_WW.bin";
// constexpr char kTxPwrUSPath[] = "nxpfmac/txpower_US.bin";
constexpr char kWlanCalDataPath[] = "nxpfmac/WlanCalData_sd8987.conf";

Device::Device(zx_device_t *parent)
    : DeviceType(parent),
      outgoing_dir_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {
  defer_rx_work_event_ =
      event_handler_.RegisterForEvent(MLAN_EVENT_ID_DRV_DEFER_RX_WORK, [this](pmlan_event event) {
        if (data_plane_) {
          data_plane_->DeferRxWork();
        }
      });
  flush_rx_work_event_ =
      event_handler_.RegisterForEvent(MLAN_EVENT_ID_DRV_FLUSH_RX_WORK, [this](pmlan_event event) {
        if (data_plane_) {
          data_plane_->FlushRxWork();
        }
      });
  defer_handling_event_ =
      event_handler_.RegisterForEvent(MLAN_EVENT_ID_DRV_DEFER_HANDLING, [this](pmlan_event event) {
        if (bus_) {
          bus_->TriggerMainProcess();
        }
      });
}

void Device::DdkInit(ddk::InitTxn txn) {
  bool fw_init_pending = false;
  const zx_status_t status = [&]() -> zx_status_t {
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        {}, "nxpfmac-sdio-wlanphy",
        [&](fdf_dispatcher_t *) { sync_completion_signal(&fidl_dispatcher_completion_); });
    if (dispatcher.is_error()) {
      NXPF_ERR("Failed to create fdf dispatcher: %s", dispatcher.status_string());
      return dispatcher.status_value();
    }
    fidl_dispatcher_ = std::move(*dispatcher);

    // Store the default driver dispatcher for creating interface.
    driver_async_dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    zx_status_t status = Init(&mlan_device_, &bus_);
    if (status != ZX_OK) {
      NXPF_ERR("nxpfmac: Init failed: %s", zx_status_get_string(status));
      return status;
    }
    if (mlan_device_.callbacks.moal_read_reg == nullptr ||
        mlan_device_.callbacks.moal_write_reg == nullptr ||
        mlan_device_.callbacks.moal_read_data_sync == nullptr ||
        mlan_device_.callbacks.moal_write_data_sync == nullptr) {
      NXPF_ERR("Bus initialization did not populate bus specific callbacks");
      return ZX_ERR_INTERNAL;
    }
    if (mlan_device_.pmoal_handle == nullptr) {
      NXPF_ERR("Bus initialization did not populate moal handle");
      return ZX_ERR_INTERNAL;
    }
    context_ = static_cast<DeviceContext *>(mlan_device_.pmoal_handle);
    context_->device_ = this;
    context_->event_handler_ = &event_handler_;

    // Create the internal memory allocator (for mlan malloc() calls).
    status = InternalMemAllocator::Create(bus_, InternalMemAllocator::kDefaultInternalVmoSize,
                                          &internal_mem_allocator_);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to create internal mem allocator: %s", status);
      return status;
    }
    context_->internal_mem_allocator_ = internal_mem_allocator_.get();

    populate_callbacks(&mlan_device_);

    mlan_status ml_status = mlan_register(&mlan_device_, &mlan_adapter_);
    if (ml_status != MLAN_STATUS_SUCCESS) {
      NXPF_ERR("mlan_register failed: %d", ml_status);
      return ZX_ERR_INTERNAL;
    }

    auto ioctl_adapter = IoctlAdapter::Create(mlan_adapter_, bus_);
    if (ioctl_adapter.is_error()) {
      NXPF_ERR("Failed to create ioctl adapter: %s", ioctl_adapter.status_string());
      return ioctl_adapter.status_value();
    }
    ioctl_adapter_ = std::move(ioctl_adapter.value());
    context_->ioctl_adapter_ = ioctl_adapter_.get();

    status = bus_->OnMlanRegistered(mlan_adapter_);
    if (status != ZX_OK) {
      NXPF_ERR("OnMlanRegistered failed: %s", zx_status_get_string(status));
      return status;
    }

    status = InitFirmware(&fw_init_pending);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to initialize firmware: %s", zx_status_get_string(status));
      return status;
    }

    status = RetrieveMacAddress();
    if (status != ZX_OK) {
      NXPF_ERR("Failed to retrieve MAC address: %s", zx_status_get_string(status));
      return status;
    }

    return ZX_OK;
  }();

  if (status != ZX_OK || !fw_init_pending) {
    // Initialization either failed or firmware initialization completed immediately, complete the
    // initializaiton.
    txn.Reply(status);
    return;
  }
  init_txn_ = std::move(txn);
  // Otherwise wait until firmware downloading has completed.
}

void Device::DdkRelease() {
  PerformShutdown();
  delete this;
}

void Device::DdkSuspend(ddk::SuspendTxn txn) {
  NXPF_INFO("Shutdown requested");

  PerformShutdown();

  NXPF_INFO("Shutdown completed");
  txn.Reply(ZX_OK, txn.requested_state());
}

void Device::WaitForProtocolConnection() { protocol_connected_.Wait(); }

zx_status_t Device::ServeWlanPhyImplProtocol(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  // This callback will be invoked when this service is being connected.
  auto protocol = [this](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) mutable {
    fdf::BindServer(fidl_dispatcher_.get(), std::move(server_end), this);
    protocol_connected_.Signal();
  };

  // Register the callback to handler.
  fuchsia_wlan_phyimpl::Service::InstanceHandler handler({.wlan_phy_impl = std::move(protocol)});

  // Add this service to the outgoing directory so that the child driver can connect to by calling
  // DdkConnectRuntimeProtocol().
  auto status = outgoing_dir_.AddService<fuchsia_wlan_phyimpl::Service>(std::move(handler));
  if (status.is_error()) {
    NXPF_ERR("%s(): Failed to add service to outgoing directory: %s\n", status.status_string());
    return status.error_value();
  }

  // Serve the outgoing directory to the entity that intends to open it, which is DFv1 in this case.
  auto result = outgoing_dir_.Serve(std::move(server_end));
  if (result.is_error()) {
    NXPF_ERR("%s(): Failed to serve outgoing directory: %s\n", result.status_string());
    return result.error_value();
  }

  return ZX_OK;
}

void Device::GetSupportedMacRoles(fdf::Arena &arena,
                                  GetSupportedMacRolesCompleter::Sync &completer) {
  fuchsia_wlan_common::wire::WlanMacRole
      supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles] = {};
  uint8_t supported_mac_roles_count = 0;
  IoctlRequest<mlan_ds_bss> bss_req(MLAN_IOCTL_BSS, MLAN_ACT_GET, 0,
                                    {.sub_command = MLAN_OID_BSS_ROLE});
  auto &bss_role = bss_req.UserReq().param.bss_role;

  for (uint8_t i = 0; i < MLAN_MAX_BSS_NUM; i++) {
    // Retrieve the role of BSS at index i
    bss_req.IoctlReq().bss_index = i;

    const IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&bss_req);
    if (io_status != IoctlStatus::Success) {
      NXPF_ERR("BSS role get req for bss: %d failed: %d", i, io_status);
      continue;
    }
    switch (bss_role) {
      case MLAN_BSS_TYPE_STA:
        supported_mac_roles_list[supported_mac_roles_count++] =
            fuchsia_wlan_common::wire::WlanMacRole::kClient;
        break;
      case MLAN_BSS_TYPE_UAP:
        supported_mac_roles_list[supported_mac_roles_count++] =
            fuchsia_wlan_common::wire::WlanMacRole::kAp;
        break;
      default:
        NXPF_ERR("Unsupported BSS role: %d at idx: %d", bss_role, i);
    }
  }

  auto reply_vector = fidl::VectorView<fuchsia_wlan_common::wire::WlanMacRole>::FromExternal(
      supported_mac_roles_list, supported_mac_roles_count);
  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(fidl_arena);
  builder.supported_mac_roles(reply_vector);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void Device::CreateIface(CreateIfaceRequestView request, fdf::Arena &arena,
                         CreateIfaceCompleter::Sync &completer) {
  std::lock_guard<std::mutex> lock(lock_);

  if (!request->has_role() || !request->has_mlme_channel()) {
    NXPF_ERR("Missing role(%s) and/or channel(%s)", request->has_role() ? "true" : "false",
             request->has_mlme_channel() ? "true" : "false");
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  uint16_t iface_id = 0;
  uint8_t mac_address[ETH_ALEN];
  const char *name;

  switch (request->role()) {
    case fuchsia_wlan_common::wire::WlanMacRole::kClient: {
      if (client_interface_ != nullptr) {
        NXPF_ERR("Client interface already exists");
        completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_EXISTS);
        return;
      }
      if (request->has_init_sta_addr()) {
        memcpy(mac_address, request->init_sta_addr().data(), ETH_ALEN);
      } else {
        memcpy(mac_address, mac_address_, ETH_ALEN);
      }
      name = kClientInterfaceName;
      iface_id = kClientInterfaceId;
      break;
    }
    case fuchsia_wlan_common::wire::WlanMacRole::kAp: {
      if (ap_interface_ != nullptr) {
        NXPF_ERR("AP interface already exists");
        completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_EXISTS);
        return;
      }
      if (!request->has_init_sta_addr()) {
        NXPF_ERR("SoftAP requires mac address to be set");
        completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
      memcpy(mac_address, request->init_sta_addr().data(), ETH_ALEN);
      name = kApInterfaceName;
      iface_id = kApInterfaceId;
      break;
    };
    default: {
      NXPF_ERR("MAC role %u not supported", request->role());
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
      return;
    }
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    NXPF_ERR("failed to create endpoints: %s\n", endpoints.status_string());
    return;
  }

  WlanInterface *interface = nullptr;
  // Create the iface and serve the protocol wlan_fullmac_impl protocol in its outgoing directory in
  // the default driver dispatcher, because they contain outgoing directory operations.
  libsync::Completion served;
  async::PostTask(driver_async_dispatcher_, [&]() {
    zx_status_t status =
        WlanInterface::Create(parent(), iface_id, request->role(), context_, mac_address,
                              std::move(request->mlme_channel()), &interface);
    if (status != ZX_OK) {
      NXPF_ERR("Could not create client interface: %s", zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }

    status = interface->ServeWlanFullmacImplProtocol(std::move(endpoints->server));
    if (status != ZX_OK) {
      NXPF_ERR("failed to serve WlanFullmacImpl service: %s\n", zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }
    served.Signal();
  });
  served.Wait();

  std::array<const char *, 1> offers{
      fuchsia_wlan_fullmac::Service::Name,
  };

  zx_status_t status = interface->DdkAdd(::ddk::DeviceAddArgs(name)
                                             .set_proto_id(ZX_PROTOCOL_WLAN_FULLMAC_IMPL)
                                             .set_runtime_service_offers(offers)
                                             .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    NXPF_ERR("Failed to add fullmac device: %s", zx_status_get_string(status));
    return;
  }

  // The lifecycle of `interface` is owned by the devhost.
  switch (iface_id) {
    case kClientInterfaceId:
      client_interface_ = interface;
      break;
    case kApInterfaceId:
      ap_interface_ = interface;
      break;
    default:
      NXPF_ERR("iface id not supported: %u", iface_id);
      return;
  }

  fidl::Arena fidl_arena;
  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena);
  builder.iface_id(iface_id);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void Device::DestroyIface(DestroyIfaceRequestView request, fdf::Arena &arena,
                          DestroyIfaceCompleter::Sync &completer) {
  std::lock_guard<std::mutex> lock(lock_);

  NXPF_INFO("Destroying interface %u", request->iface_id());
  switch (request->iface_id()) {
    case kClientInterfaceId: {
      if (client_interface_ == nullptr) {
        NXPF_ERR("Client interface %u unavailable, skipping destroy.", request->iface_id());
        completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
        return;
      }
      // Remove the interface and asynchronously reply to the request once the removal is complete.
      client_interface_->Remove(
          [completer = completer.ToAsync(), arena = std::move(arena)]() mutable {
            completer.buffer(arena).ReplySuccess();
          });
      client_interface_ = nullptr;
      return;
    }
    case kApInterfaceId: {
      if (ap_interface_ == nullptr) {
        NXPF_ERR("AP interface %u unavailable, skipping destroy.", request->iface_id());
        completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
        return;
      }
      // Remove the interface and asynchronously reply to the request once the removal is complete.
      ap_interface_->Remove([completer = completer.ToAsync(), arena = std::move(arena)]() mutable {
        completer.buffer(arena).ReplySuccess();
      });
      ap_interface_ = nullptr;
      return;
    }
    default: {
      NXPF_ERR("AP interface %u unavailable, skipping destroy.", request->iface_id());
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
  }
  // At this point we MUST have either replied or used ToAsync with the intent of replying
  // asynchronously.
  if (completer.is_reply_needed()) {
    NXPF_ERR("Completed DestroyIface without completing request");
  }
}

// This function attempts to load the power file, rgpowerxx.bin for the given country and if not
// found, attempts to load the file, txpowerxx.bin.
zx_status_t Device::LoadPowerFile(char country_code[3], std::vector<uint8_t> *pwr_data_out) {
  constexpr char kRgPwrXXPath[] = "nxpfmac/rgpower_";
  constexpr char kTxPwrXXPath[] = "nxpfmac/txpower_";
  // Attempt to load the corresponding rgpower file first
  std::string tx_pwr_file_name = std::string(kRgPwrXXPath) + country_code + ".bin";
  zx_status_t status = LoadFirmwareData(tx_pwr_file_name.c_str(), pwr_data_out);
  if (status != ZX_OK) {
    NXPF_INFO("Failed to load Power file:%s err: %s", tx_pwr_file_name.c_str(),
              zx_status_get_string(status));
    // rgpowerxx.bin is not found. Attempt to load the corresponding txpowerxx.bin file next
    tx_pwr_file_name = std::string(kTxPwrXXPath) + country_code + ".bin";

    status = LoadFirmwareData(tx_pwr_file_name.c_str(), pwr_data_out);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to load Tx Power file:%s err: %s", tx_pwr_file_name.c_str(),
               zx_status_get_string(status));
    }
  }
  return status;
}

// This function attempts to load the given country code's power file and if that
// fails it reverts the country code to WW. Once the power file download succeeds,
// MLAN_OID_MISC_COUNTRY_CODE ioctl to FW to set the country code.
zx_status_t Device::SetCountryCodeInFw(char country[3]) {
  std::vector<uint8_t> txpwr_data;

  // Attempt to load the power file for the given country
  zx_status_t status = LoadPowerFile(country, &txpwr_data);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to load Tx Power file for country: %s err: %s", country,
             zx_status_get_string(status));
    // If unable to find the power file for the given country, load the file for WW
    strcpy(country, "WW");
    status = LoadPowerFile(country, &txpwr_data);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to load Tx Power file for country: %s err: %s", country,
               zx_status_get_string(status));
      return status;
    }
  }

  // Send the power file data to FW.
  status = process_hostcmd_cfg(context_->ioctl_adapter_, (char *)txpwr_data.data(),
                               (t_size)txpwr_data.size());
  if (status != ZX_OK) {
    NXPF_ERR("process hostcmd failed");
    return status;
  }

  // Once the power file has been downloaded, send the country code ioctl to FW.
  // Bss index shouldn't matter here, set it to zero.
  IoctlRequest<mlan_ds_misc_cfg> ioctl_request(
      MLAN_IOCTL_MISC_CFG, MLAN_ACT_SET, 0,
      mlan_ds_misc_cfg{
          .sub_command = MLAN_OID_MISC_COUNTRY_CODE,
          .param{.country_code{.country_code{(uint8_t)country[0], (uint8_t)country[1], '\0'}}}});

  IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&ioctl_request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to set country %s: status %d", country, io_status);
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

void Device::SetCountry(SetCountryRequestView request, fdf::Arena &arena,
                        SetCountryCompleter::Sync &completer) {
  char country[3] = {};

  memcpy(country, request->alpha2().data_, 2);
  zx_status_t status = SetCountryCodeInFw(country);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to set country: %s in FW err: %s", country, zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void Device::GetCountry(fdf::Arena &arena, GetCountryCompleter::Sync &completer) {
  // Bss index shouldn't matter here, set it to zero.
  IoctlRequest<mlan_ds_misc_cfg> ioctl_request(
      MLAN_IOCTL_MISC_CFG, MLAN_ACT_GET, 0,
      mlan_ds_misc_cfg{.sub_command = MLAN_OID_MISC_COUNTRY_CODE});

  IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&ioctl_request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to get country: %d", io_status);
    completer.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
    return;
  }

  fidl::Array<uint8_t, fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len> alpha2;
  memcpy(alpha2.begin(), ioctl_request.UserReq().param.country_code.country_code,
         decltype(alpha2)::size());

  completer.buffer(arena).ReplySuccess(
      ::fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2(alpha2));
}

void Device::ClearCountry(fdf::Arena &arena, ClearCountryCompleter::Sync &completer) {
  // Set country code to WW.
  char country[3] = {'W', 'W', 0};
  zx_status_t status = SetCountryCodeInFw(country);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to set country: %s in FW err: %s", country, zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void Device::SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena &arena,
                              SetPowerSaveModeCompleter::Sync &completer) {
  NXPF_ERR("Not supported");
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Device::GetPowerSaveMode(fdf::Arena &arena, GetPowerSaveModeCompleter::Sync &completer) {
  NXPF_ERR("Not supported");
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Device::OnEapolTransmitted(wlan::drivers::components::Frame &&frame, zx_status_t status) {
  auto eth = reinterpret_cast<ethhdr *>(frame.Data());

  switch (frame.PortId()) {
    case kClientInterfaceId:
      client_interface_->OnEapolTransmitted(status, eth->h_dest);
      break;
    case kApInterfaceId:
      ap_interface_->OnEapolTransmitted(status, eth->h_dest);
      break;
    default:
      NXPF_ERR("EAPOL transmitted on unknown interface %u", frame.PortId());
      break;
  }
}

void Device::OnEapolReceived(wlan::drivers::components::Frame &&frame) {
  switch (frame.PortId()) {
    case kClientInterfaceId:
      client_interface_->OnEapolResponse(std::move(frame));
      break;
    case kApInterfaceId:
      ap_interface_->OnEapolResponse(std::move(frame));
      break;
    default:
      NXPF_ERR("EAPOL received on unknown interface %u", frame.PortId());
      break;
  }
}

void Device::OnFirmwareInitComplete(zx_status_t status) {
  if (status == ZX_OK) {
    NXPF_INFO("Firmware initialization complete");
  } else {
    NXPF_ERR("Firmware initialization failed: %s", zx_status_get_string(status));
  }
  if (!init_txn_.has_value()) {
    NXPF_ERR("No initialization transaction in progress");
    return;
  }
  init_txn_->Reply(status);
  init_txn_.reset();
}

void Device::OnFirmwareShutdownComplete(zx_status_t status) {
  if (status == ZX_OK) {
    NXPF_INFO("Firmware shutdown complete");
  } else {
    NXPF_ERR("Firmware shutdown failed: %s", zx_status_get_string(status));
  }
}

void Device::PerformShutdown() {
  // Shut down the fidl dispatcher first. We don't want any calls coming in while this is happening.
  fidl_dispatcher_.ShutdownAsync();
  zx_status_t status = sync_completion_wait(&fidl_dispatcher_completion_, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to wait for fdf dispatcher to shutdown: %s", zx_status_get_string(status));
    // Keep going and shut everything else down.
  }

  if (init_txn_.has_value()) {
    // We must reply to the initialization request or it will trigger an assert. If we get to this
    // point without completing initialization we should consider it a failure.
    NXPF_WARN("Shutting down before init completed.");
    init_txn_->Reply(ZX_ERR_INTERNAL);
    init_txn_.reset();
  }

  if (mlan_adapter_) {
    // The MLAN shutdown functions still rely on the IRQ thread to be running when shutting down.
    // Shut down MLAN first, then everything else.
    mlan_status ml_status = mlan_shutdown_fw(mlan_adapter_);
    if (ml_status != MLAN_STATUS_SUCCESS) {
      NXPF_ERR("Failed to shutdown firmware: %d", ml_status);
    }

    // Shut down the bus specific device. This should stop any IRQ thread from running, otherwise it
    // might access data that will go away during mlan_unregister.
    Shutdown();

    ml_status = mlan_unregister(mlan_adapter_);
    if (ml_status != MLAN_STATUS_SUCCESS) {
      NXPF_ERR("Failed to unregister mlan: %d", ml_status);
      // Don't stop on this error, we need to shut down everything else anyway.
    }
    mlan_adapter_ = nullptr;
  }
}

zx_status_t Device::InitFirmware(bool *out_is_pending) {
  std::vector<uint8_t> firmware_data;
  zx_status_t status = LoadFirmwareData(kFirmwarePath, &firmware_data);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to load firmware data from '%s': %s", kFirmwarePath,
             zx_status_get_string(status));
    return status;
  }

  std::vector<uint8_t> tx_power_data;
  status = LoadFirmwareData(kTxPwrWWPath, &tx_power_data);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to load calibration data from '%s': %s", kTxPwrWWPath,
             zx_status_get_string(status));
    return status;
  }

  std::vector<uint8_t> calibration_data;
  status = LoadFirmwareData(kWlanCalDataPath, &calibration_data);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to load calibration data from '%s': %s", kWlanCalDataPath,
             zx_status_get_string(status));
    return status;
  }

  mlan_init_param init_param = {
      .ptxpwr_data_buf = tx_power_data.data(),
      .txpwr_data_len = static_cast<uint32_t>(tx_power_data.size()),
      .pcal_data_buf = calibration_data.data(),
      .cal_data_len = static_cast<uint32_t>(calibration_data.size()),
  };
  mlan_status ml_status = mlan_set_init_param(mlan_adapter_, &init_param);
  if (ml_status != MLAN_STATUS_SUCCESS) {
    NXPF_ERR("mlan_set_init_param failed: %d", ml_status);
    return ZX_ERR_INTERNAL;
  }

  mlan_fw_image fw = {
      .pfw_buf = firmware_data.data(),
      .fw_len = static_cast<uint32_t>(firmware_data.size()),
      .fw_reload = false,
  };
  ml_status = mlan_dnld_fw(mlan_adapter_, &fw);
  if (ml_status != MLAN_STATUS_SUCCESS) {
    NXPF_ERR("mlan_dnld_fw failed: %d", ml_status);
    return ZX_ERR_INTERNAL;
  }

  ml_status = mlan_init_fw(mlan_adapter_);
  // Firmware initialization could be asynchronous and in that case will return pending. When
  // initialization is complete moal_init_fw_complete will be called.
  if (ml_status != MLAN_STATUS_SUCCESS && ml_status != MLAN_STATUS_PENDING) {
    NXPF_ERR("mlan_init_fw failed: %d", ml_status);
    return ZX_ERR_INTERNAL;
  }
  *out_is_pending = ml_status == MLAN_STATUS_PENDING;

  status = bus_->OnFirmwareInitialized();
  if (status != ZX_OK) {
    NXPF_ERR("OnFirmwareInitialized failed: %s", zx_status_get_string(status));
    return status;
  }

  status = DataPlane::Create(parent(), this, bus_, mlan_adapter_, &data_plane_);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to create data plane: %s", zx_status_get_string(status));
    return status;
  }
  context_->data_plane_ = data_plane_.get();

  return ZX_OK;
}

zx_status_t Device::LoadFirmwareData(const char *path, std::vector<uint8_t> *data_out) {
  zx::vmo vmo;
  size_t vmo_size = 0;
  zx_status_t status = LoadFirmware(path, &vmo, &vmo_size);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to load firmware data '%s': %s", path, zx_status_get_string(status));
    return status;
  }
  if (vmo_size > std::numeric_limits<uint32_t>::max()) {
    NXPF_ERR("Firmware data in '%s' exceeds maximum size of 4 GiB", path);
    return ZX_ERR_FILE_BIG;
  }

  std::vector<uint8_t> data(vmo_size);

  status = vmo.read(data.data(), 0, data.size());
  if (status != ZX_OK) {
    NXPF_ERR("Failed to read firmware data in '%s' from VMO: %s", path,
             zx_status_get_string(status));
    return status;
  }

  *data_out = std::move(data);

  return ZX_OK;
}

zx_status_t Device::RetrieveMacAddress() {
  // MAC address is only 6 bytes, but it is rounded up to 8 in the ZBI
  uint8_t bootloader_macaddr[8] = {};
  size_t actual_len = 0;
  zx_status_t status = DdkGetMetadata(DEVICE_METADATA_MAC_ADDRESS, bootloader_macaddr,
                                      sizeof(bootloader_macaddr), &actual_len);
  if (status == ZX_OK) {
    memcpy(mac_address_, bootloader_macaddr, ETH_ALEN);
    return ZX_OK;
  }
  NXPF_ERR("Failed to get MAC address metadata: %s", zx_status_get_string(status));

  // Fall back on random MAC address
  if (getentropy(mac_address_, ETH_ALEN) == 0) {
    mac_address_[0] &= 0xfe;  // bit 0: 0 = unicast
    mac_address_[0] |= 0x02;  // bit 1: 1 = locally-administered
    return ZX_OK;
  }
  NXPF_ERR("Failed to randomly generate MAC address");

  // Then fall back on the firmware address
  IoctlRequest<mlan_ds_bss> request(MLAN_IOCTL_BSS, MLAN_ACT_GET, 0,
                                    {.sub_command = MLAN_OID_BSS_MAC_ADDR});
  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status == IoctlStatus::Success) {
    memcpy(mac_address_, request.UserReq().param.mac_addr, ETH_ALEN);
    return ZX_OK;
  }
  NXPF_ERR("Failed to retrieve MAC address from firmware: %d", io_status);
  return ZX_ERR_IO;
}

}  // namespace wlan::nxpfmac
