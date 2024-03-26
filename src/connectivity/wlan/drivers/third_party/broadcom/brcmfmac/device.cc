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

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/device.h"

#include <lib/fdf/dispatcher.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/status.h>

#include <wlan/common/ieee80211.h>

#include "fidl/fuchsia.wlan.phyimpl/cpp/wire_types.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/cfg80211.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/common.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/debug.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/feature.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/wlan_interface.h"

namespace wlan {
namespace brcmfmac {
namespace {

constexpr char kClientInterfaceName[] = "brcmfmac-wlan-fullmac-client";
constexpr uint8_t kClientInterfaceId = 0;
constexpr char kApInterfaceName[] = "brcmfmac-wlan-fullmac-ap";
constexpr uint8_t kApInterfaceId = 1;
constexpr uint8_t kMaxBufferParts = 1;
constexpr char kNetDevDriverName[] = "brcmfmac-netdev";
}  // namespace

Device::Device()
    : brcmf_pub_(std::make_unique<brcmf_pub>()),
      client_interface_(nullptr),
      ap_interface_(nullptr) {
  brcmf_pub_->device = this;
  for (auto& entry : brcmf_pub_->if2bss) {
    entry = BRCMF_BSSIDX_INVALID;
  }

  // Initialize the recovery trigger for driver, shared by all buses' devices.
  auto recovery_start_callback = std::make_shared<std::function<zx_status_t()>>();
  *recovery_start_callback = std::bind(&brcmf_schedule_recovery_worker, brcmf_pub_.get());
  brcmf_pub_->recovery_trigger =
      std::make_unique<wlan::brcmfmac::RecoveryTrigger>(recovery_start_callback);
}

Device::~Device() {}

void Device::Shutdown() {
  if (brcmf_pub_) {
    // Shut down the default WorkQueue here to ensure that its dispatcher is shutdown properly even
    // if the Device object's destructor is not called.
    brcmf_pub_->default_wq.Shutdown();
  }
}

zx_status_t Device::AddWlanPhyImplService() {
  // Add the service contains WlanphyImpl protocol to outgoing directory.
  auto wlanphyimpl = [this](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) {
    // Call the handler inherited from WlanPhyImplDevice.
    // Note: The same dispatcher here is used for fullmac device, will it affect the data path
    // performance?
    ServiceConnectHandler(GetDriverDispatcher(), std::move(server_end));
  };

  fuchsia_wlan_phyimpl::Service::InstanceHandler wlanphy_service_handler(
      {.wlan_phy_impl = wlanphyimpl});

  auto status =
      Outgoing()->AddService<fuchsia_wlan_phyimpl::Service>(std::move(wlanphy_service_handler));
  if (status.is_error()) {
    BRCMF_ERR("Failed to add service to outgoing directory: %s", status.status_string());
    return status.status_value();
  }

  return ZX_OK;
}

zx_status_t Device::InitWlanPhyImpl() {
  fidl::Arena arena;
  fidl::VectorView<fuchsia_driver_framework::wire::Offer> offers(arena, 1);
  offers[0] = fdf::MakeOffer2<fuchsia_wlan_phyimpl::Service>(arena);

  auto args = fdf::wire::NodeAddArgs::Builder(arena)
                  .name("brcmfmac-wlanphyimpl")
                  .offers2(offers)
                  .Build();

  auto endpoints = fidl::CreateEndpoints<fdf::NodeController>();
  if (endpoints.is_error()) {
    BRCMF_ERR("CreateEndPoints failed: %s", endpoints.status_string());
    return endpoints.error_value();
  }

  // Adding wlanphy child node. Doing a sync version here to reduce chaos.
  auto result = GetParentNode().sync()->AddChild(std::move(args), std::move(endpoints->server), {});

  if (!result.ok()) {
    BRCMF_ERR("Add wlanphy node error due to FIDL error on protocol [Node]: %s",
              result.status_string());
    return result.status();
  }

  if (result->is_error()) {
    BRCMF_ERR("Add wlanphy node error: %u", static_cast<uint32_t>(result->error_value()));
    return result.status();
  }

  wlanphy_controller_client_.Bind(std::move(endpoints->client),
                                  fdf::Dispatcher::GetCurrent()->async_dispatcher(), this);

  return ZX_OK;
}

void Device::CreateNetDevice() {
  network_device_ = std::make_unique<::wlan::drivers::components::NetworkDevice>(this);
}

zx_status_t Device::AddNetworkDevice(const char* deviceName) {
  fidl::Arena arena;
  auto property = fdf::MakeProperty(arena, BIND_PROTOCOL, ZX_PROTOCOL_NETWORK_DEVICE_IMPL);
  auto args = fdf::wire::NodeAddArgs::Builder(arena)
                  .name(arena, deviceName)
                  .properties(fidl::VectorView<fdf::wire::NodeProperty>::FromExternal(&property, 1))
                  .offers2(GetCompatServer().CreateOffers2(arena))
                  .Build();
  auto endpoints = fidl::CreateEndpoints<fdf::NodeController>();
  if (endpoints.is_error()) {
    BRCMF_ERR("Create Endpoints failed: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  // Add the netdevice child node for the node that this driver is binding to. Doing a sync version
  // here to reduce chaos.
  auto result = GetParentNode().sync()->AddChild(std::move(args), std::move(endpoints->server), {});
  if (!result.ok()) {
    BRCMF_ERR("Add controller node error due to FIDL error on protocol [Node]: %s",
              result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    BRCMF_ERR("Add controller node error: %u", static_cast<uint32_t>(result->error_value()));
    return ZX_ERR_INTERNAL;
  }
  NetDev()->Init(std::move(endpoints->client), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  return ZX_OK;
}

zx_status_t Device::InitDevice() {
  zx_status_t status = AddNetworkDevice(kNetDevDriverName);
  if (status != ZX_OK) {
    BRCMF_ERR("Failed to initialize network device %s", zx_status_get_string(status));
    return status;
  }
  status = BusInit();
  if (status != ZX_OK) {
    BRCMF_ERR("Init failed: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void Device::InitPhyDevice() {
  // Setup the WlanPhyImpl Service
  zx_status_t status;

  if ((status = AddWlanPhyImplService()) != ZX_OK) {
    BRCMF_ERR("ServeRuntimeProtocolForV1Devices failed: %s", zx_status_get_string(status));
    NetDevInitReply(status);
    return;
  }
  if ((status = InitWlanPhyImpl()) != ZX_OK) {
    BRCMF_ERR("Init WlanPhyImpl failed: %s", zx_status_get_string(status));
    NetDevInitReply(status);
    return;
  }
  NetDevInitReply(ZX_OK);
}

brcmf_pub* Device::drvr() { return brcmf_pub_.get(); }

const brcmf_pub* Device::drvr() const { return brcmf_pub_.get(); }

void Device::GetSupportedMacRoles(fdf::Arena& arena,
                                  GetSupportedMacRolesCompleter::Sync& completer) {
  BRCMF_DBG(WLANPHY, "Received request for supported MAC roles from SME dfv2");
  fuchsia_wlan_common::wire::WlanMacRole
      supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles] = {};
  uint8_t supported_mac_roles_count = 0;
  zx_status_t status = WlanInterface::GetSupportedMacRoles(
      brcmf_pub_.get(), supported_mac_roles_list, &supported_mac_roles_count);
  if (status != ZX_OK) {
    BRCMF_ERR("Device::GetSupportedMacRoles() failed to get supported mac roles: %s\n",
              zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  if (supported_mac_roles_count > fuchsia_wlan_common::wire::kMaxSupportedMacRoles) {
    BRCMF_ERR(
        "Device::GetSupportedMacRoles() Too many mac roles returned from brcmfmac driver. Number "
        "of supported max roles got "
        "from driver is %u, but the limitation is: %u\n",
        supported_mac_roles_count, fuchsia_wlan_common::wire::kMaxSupportedMacRoles);
    completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  auto reply_vector = fidl::VectorView<fuchsia_wlan_common::wire::WlanMacRole>::FromExternal(
      supported_mac_roles_list, supported_mac_roles_count);
  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(fidl_arena);
  builder.supported_mac_roles(reply_vector);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void Device::CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                         CreateIfaceCompleter::Sync& completer) {
  std::lock_guard<std::mutex> lock(lock_);
  BRCMF_INFO("Device::CreateIface() creating interface started dfv2");

  if (!request->has_role() || !request->has_mlme_channel()) {
    BRCMF_ERR("Device::CreateIface() missing information in role(%u), channel(%u)",
              request->has_role(), request->has_mlme_channel());
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx_status_t status = ZX_OK;
  wireless_dev* wdev = nullptr;
  uint16_t iface_id = 0;

  switch (request->role()) {
    case fuchsia_wlan_common::wire::WlanMacRole::kClient: {
      if (client_interface_ != nullptr) {
        BRCMF_ERR("Device::CreateIface() client interface already exists");
        completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
        return;
      }

      // If we are operating with manufacturing firmware ensure SoftAP IF is also not present
      if (brcmf_feat_is_enabled(brcmf_pub_.get(), BRCMF_FEAT_MFG)) {
        if (ap_interface_ != nullptr) {
          BRCMF_ERR("Simultaneous mode not supported in mfg FW - Ap IF already exists");
          completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
          return;
        }
      }

      if ((status = brcmf_cfg80211_add_iface(brcmf_pub_.get(), kClientInterfaceName, nullptr,
                                             request, &wdev)) != ZX_OK) {
        BRCMF_ERR("Device::CreateIface() failed to create Client interface, %s",
                  zx_status_get_string(status));
        completer.buffer(arena).ReplyError(status);
        return;
      }

      zx::result create_result =
          WlanInterface::Create(this, kClientInterfaceName, wdev, request->role());
      if (create_result.is_error()) {
        BRCMF_ERR("Failed to create WlanInterface: %s", create_result.status_string());
        completer.buffer(arena).ReplyError(create_result.status_value());
        return;
      }

      client_interface_ = std::move(create_result.value());
      iface_id = kClientInterfaceId;

      break;
    }

    case fuchsia_wlan_common::wire::WlanMacRole::kAp: {
      if (ap_interface_ != nullptr) {
        BRCMF_ERR("Device::CreateIface() AP interface already exists");
        completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
        return;
      }

      // If we are operating with manufacturing firmware ensure client IF is also not present
      if (brcmf_feat_is_enabled(brcmf_pub_.get(), BRCMF_FEAT_MFG)) {
        if (client_interface_ != nullptr) {
          BRCMF_ERR("Simultaneous mode not supported in mfg FW - Client IF already exists");
          completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
          return;
        }
      }

      if ((status = brcmf_cfg80211_add_iface(brcmf_pub_.get(), kApInterfaceName, nullptr, request,
                                             &wdev)) != ZX_OK) {
        BRCMF_ERR("Device::CreateIface() failed to create AP interface, %s",
                  zx_status_get_string(status));
        completer.buffer(arena).ReplyError(status);
        return;
      }

      zx::result create_result =
          WlanInterface::Create(this, kApInterfaceName, wdev, request->role());
      if (create_result.is_error()) {
        BRCMF_ERR("Failed to create WlanInterface: %s", create_result.status_string());
        completer.buffer(arena).ReplyError(create_result.status_value());
        return;
      }

      ap_interface_ = std::move(create_result.value());
      iface_id = kApInterfaceId;

      break;
    }

    default: {
      BRCMF_ERR("Device::CreateIface() MAC role %d not supported",
                fidl::ToUnderlying(request->role()));
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
      return;
    }
  }

  // Log the new iface's role, name, and MAC address
  net_device* ndev = wdev->netdev;

  const char* role = request->role() == fuchsia_wlan_common::wire::WlanMacRole::kClient ? "client"
                     : request->role() == fuchsia_wlan_common::wire::WlanMacRole::kAp   ? "ap"
                     : request->role() == fuchsia_wlan_common::wire::WlanMacRole::kMesh
                         ? "mesh"
                         : "unknown type";
  BRCMF_DBG(WLANPHY, "Created %s iface with netdev:%s id:%d", role, ndev->name, iface_id);
#if !defined(NDEBUG)
  const uint8_t* mac_addr = ndev_to_if(ndev)->mac_addr;
  BRCMF_DBG(WLANPHY, "  address: " FMT_MAC, FMT_MAC_ARGS(mac_addr));
#endif /* !defined(NDEBUG) */

  fidl::Arena fidl_arena;
  auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena);
  builder.iface_id(iface_id);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void Device::DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                          DestroyIfaceCompleter::Sync& completer) {
  zx_status_t status;
  std::lock_guard<std::mutex> lock(lock_);

  if (!request->has_iface_id()) {
    BRCMF_ERR("Device::DestroyIface() invoked without valid iface_id");
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  uint16_t iface_id = request->iface_id();
  BRCMF_DBG(WLANPHY, "Destroying interface %d", iface_id);
  switch (iface_id) {
    case kClientInterfaceId: {
      if (client_interface_ == nullptr) {
        BRCMF_WARN("Client interface not found");
        completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
        return;
      }
      status = client_interface_->DestroyIface();
      if (status == ZX_OK) {
        BRCMF_DBG(WLANPHY, "Interface %d destroyed successfully", iface_id);
        client_interface_.reset();
        completer.buffer(arena).ReplySuccess();
      } else {
        // Don't reset client_interface_ here since we failed to delete it.
        BRCMF_ERR("Device::DestroyIface() Error destroying Client interface : %s",
                  zx_status_get_string(status));
        completer.buffer(arena).ReplyError(status);
      }
      return;
    }
    case kApInterfaceId: {
      if (ap_interface_ == nullptr) {
        BRCMF_ERR("Softap interface not found");
        completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
        return;
      }
      status = ap_interface_->DestroyIface();
      if (status == ZX_OK) {
        BRCMF_DBG(WLANPHY, "Interface %d destroyed successfully", iface_id);
        ap_interface_.reset();
        completer.buffer(arena).ReplySuccess();
      } else {
        // Don't reset ap_interface_ here since we failed to delete it.
        BRCMF_ERR("Device::DestroyIface() Error destroying Client interface : %s",
                  zx_status_get_string(status));
        completer.buffer(arena).ReplyError(status);
      }
      return;
    }
    default: {
      BRCMF_ERR("Device::DestroyIface() Unknown interface id: %d", iface_id);
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
  }
}

void Device::SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                        SetCountryCompleter::Sync& completer) {
  BRCMF_DBG(WLANPHY, "Setting country code dfv2");
  if (!request->is_alpha2()) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    BRCMF_ERR("Device::SetCountry() Invalid input format of country code.");
    return;
  }
  const auto country = fuchsia_wlan_phyimpl_wire::WlanPhyCountry::WithAlpha2(request->alpha2());
  zx_status_t status = WlanInterface::SetCountry(brcmf_pub_.get(), &country);
  if (status != ZX_OK) {
    BRCMF_ERR("Device::SetCountry() Failed Set country : %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess();
}

void Device::ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) {
  BRCMF_DBG(WLANPHY, "Clearing country dfv2");
  zx_status_t status = WlanInterface::ClearCountry(brcmf_pub_.get());
  if (status != ZX_OK) {
    BRCMF_ERR("Device::ClearCountry() Failed Clear country : %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess();
}

void Device::GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) {
  BRCMF_DBG(WLANPHY, "Received request for country from SME dfv2");
  uint8_t cc_code[fuchsia_wlan_phyimpl_wire::kWlanphyAlpha2Len];

  zx_status_t status = WlanInterface::GetCountry(brcmf_pub_.get(), cc_code);
  if (status != ZX_OK) {
    BRCMF_ERR("Device::GetCountry() Failed Get country : %s", zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  auto country = fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2({cc_code[0], cc_code[1]});
  BRCMF_INFO("Get country code: %c%c", country.alpha2()[0], country.alpha2()[1]);

  completer.buffer(arena).ReplySuccess(country);
}

void Device::SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                              SetPowerSaveModeCompleter::Sync& completer) {
  BRCMF_DBG(WLANPHY, "Setting power save mode dfv2");
  if (!request->has_ps_mode()) {
    BRCMF_ERR("Device::SetPowerSaveMode() invoked without ps_mode");
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx_status_t status = brcmf_set_power_save_mode(brcmf_pub_.get(), request->ps_mode());
  if (status != ZX_OK) {
    BRCMF_ERR("Device::SetPowerSaveMode() failed setting ps mode : %s",
              zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess();
}

void Device::GetPowerSaveMode(fdf::Arena& arena, GetPowerSaveModeCompleter::Sync& completer) {
  BRCMF_DBG(WLANPHY, "Received request for PS mode from SME dfv2");
  fuchsia_wlan_common_wire::PowerSaveType ps_mode;
  zx_status_t status = brcmf_get_power_save_mode(brcmf_pub_.get(), &ps_mode);
  if (status != ZX_OK) {
    BRCMF_ERR("Device::GetPowerSaveMode() Get Power Save Mode failed");
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplGetPowerSaveModeResponse::Builder(fidl_arena);
  builder.ps_mode(ps_mode);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void Device::ServiceConnectHandler(fdf_dispatcher_t* dispatcher,
                                   fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) {
  bindings_.AddBinding(dispatcher, std::move(server_end), this, [](fidl::UnbindInfo info) {
    if (!info.is_user_initiated()) {
      BRCMF_ERR("WlanPhyImpl binding unexpectedly closed: %s", info.lossy_description());
    }
  });
}

void Device::NetDevInitReply(zx_status_t status) {
  if (!netdev_init_txn_.has_value()) {
    BRCMF_ERR("NetDev Init Txn is not valid");
    return;
  }
  netdev_init_txn_.value().Reply(static_cast<int32_t>(status));
  netdev_init_txn_.reset();
}

void Device::NetDevInit(wlan::drivers::components::NetworkDevice::Callbacks::InitTxn txn) {
  netdev_init_txn_.emplace(std::move(txn));
  zx_status_t status = async::PostTask(fdf_dispatcher_get_async_dispatcher(GetDriverDispatcher()),
                                       [this] { InitPhyDevice(); });
  if (status != ZX_OK) {
    BRCMF_ERR("Async PostTask failed: %s", zx_status_get_string(status));
    NetDevInitReply(status);
  }
}

void Device::NetDevRelease() {
  // Don't need to do anything here, the release of wlanif should take care of all releasing
}

void Device::NetDevStart(wlan::drivers::components::NetworkDevice::Callbacks::StartTxn txn) {
  txn.Reply(ZX_OK);
}

void Device::NetDevStop(wlan::drivers::components::NetworkDevice::Callbacks::StopTxn txn) {
  // Flush all buffers in response to this call. They are no longer valid for use.
  brcmf_flush_buffers(drvr());
  txn.Reply();
}

void Device::NetDevGetInfo(device_impl_info_t* out_info) {
  std::lock_guard<std::mutex> lock(lock_);

  memset(out_info, 0, sizeof(*out_info));
  zx_status_t err = brcmf_get_tx_depth(drvr(), &out_info->tx_depth);
  ZX_ASSERT(err == ZX_OK);
  err = brcmf_get_rx_depth(drvr(), &out_info->rx_depth);
  ZX_ASSERT(err == ZX_OK);
  out_info->rx_threshold = out_info->rx_depth / 3;
  out_info->max_buffer_parts = kMaxBufferParts;
  out_info->max_buffer_length = ZX_PAGE_SIZE;
  out_info->buffer_alignment = ZX_PAGE_SIZE;
  out_info->min_rx_buffer_length = IEEE80211_MSDU_SIZE_MAX;

  out_info->tx_head_length = drvr()->hdrlen;
  brcmf_get_tail_length(drvr(), &out_info->tx_tail_length);
  // No hardware acceleration supported yet.
  out_info->rx_accel_count = 0;
  out_info->tx_accel_count = 0;
}

void Device::NetDevQueueTx(cpp20::span<wlan::drivers::components::Frame> frames) {
  brcmf_start_xmit(drvr(), frames);
}

void Device::NetDevQueueRxSpace(const rx_space_buffer_t* buffers_list, size_t buffers_count,
                                uint8_t* vmo_addrs[]) {
  brcmf_queue_rx_space(drvr(), buffers_list, buffers_count, vmo_addrs);
}

zx_status_t Device::NetDevPrepareVmo(uint8_t vmo_id, zx::vmo vmo, uint8_t* mapped_address,
                                     size_t mapped_size) {
  return brcmf_prepare_vmo(drvr(), vmo_id, vmo.get(), mapped_address, mapped_size);
}

void Device::NetDevReleaseVmo(uint8_t vmo_id) { brcmf_release_vmo(drvr(), vmo_id); }

void Device::NetDevSetSnoopEnabled(bool snoop) {}

void Device::DestroyAllIfaces() {
  std::lock_guard<std::mutex> lock(lock_);
  if (client_interface_) {
    zx_status_t status = client_interface_->DestroyIface();
    if (status == ZX_OK) {
      client_interface_.reset();
    } else {
      BRCMF_ERR("Device::DestroyAllIfaces() : Failed destroying client interface : %s",
                zx_status_get_string(status));
    }
  }

  if (ap_interface_) {
    zx_status_t status = ap_interface_->DestroyIface();
    if (status == ZX_OK) {
      ap_interface_.reset();
    } else {
      BRCMF_ERR("Device::DestroyAllIfaces() : Failed destroying ap interface : %s",
                zx_status_get_string(status));
    }
  }
}

}  // namespace brcmfmac
}  // namespace wlan
