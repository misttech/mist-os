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

struct InterfaceInfo {
  const char* const display_name = nullptr;
  const char* const interface_name = nullptr;
  const uint8_t interface_id = 0;
  const bool supported = false;
};

InterfaceInfo GetInterfaceInfoForRole(fuchsia_wlan_common::wire::WlanMacRole role) {
  switch (role) {
    case fuchsia_wlan_common::wire::WlanMacRole::kClient:
      return InterfaceInfo{"Client", kClientInterfaceName, kClientInterfaceId, true};
    case fuchsia_wlan_common::wire::WlanMacRole::kAp:
      return InterfaceInfo{"AP", kApInterfaceName, kApInterfaceId, true};
    case fuchsia_wlan_common::wire::WlanMacRole::kMesh:
      return InterfaceInfo{"Mesh"};
    default:
      return InterfaceInfo{"<unknown>"};
  }
}

fuchsia_wlan_common::wire::WlanMacRole GetMacRoleForInterfaceId(uint16_t interface_id) {
  switch (interface_id) {
    case kClientInterfaceId:
      return fuchsia_wlan_common::wire::WlanMacRole::kClient;
    case kApInterfaceId:
      return fuchsia_wlan_common::wire::WlanMacRole::kAp;
    default:
      return fuchsia_wlan_common::wire::WlanMacRole();
  }
}

}  // namespace

Device::Device() : brcmf_pub_(std::make_unique<brcmf_pub>()), network_device_(this) {
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

Device::~Device() = default;

void Device::Shutdown(fit::callback<void()> on_shutdown_complete) {
  if (brcmf_pub_) {
    // Shut down the default WorkQueue here to ensure that its dispatcher is shutdown properly even
    // if the Device object's destructor is not called.
    brcmf_pub_->default_wq.Shutdown();
  }

  on_netdev_shutdown_complete_ = [on_shutdown_complete = std::move(on_shutdown_complete),
                                  this]() mutable {
    if (netdev_dispatcher_.get()) {
      netdev_dispatcher_.ShutdownAsync();
      netdev_dispatcher_shutdown_.Wait();
      netdev_dispatcher_.close();
    }
    on_shutdown_complete();
  };

  if (!network_device_.Remove()) {
    // No removal needed, immediately call on_netdev_shutdown_complete to signal we're done
    on_netdev_shutdown_complete_();
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

zx_status_t Device::InitDevice(fdf::OutgoingDirectory& outgoing) {
  auto netdev_dispatcher = fdf::SynchronizedDispatcher::Create(
      {}, "brcmfmac-netdev", [this](fdf_dispatcher_t*) { netdev_dispatcher_shutdown_.Signal(); });
  if (netdev_dispatcher.is_error()) {
    BRCMF_ERR("Failed to create netdev dispatcher: %s", netdev_dispatcher.status_string());
    return netdev_dispatcher.status_value();
  }
  netdev_dispatcher_ = std::move(netdev_dispatcher.value());

  zx_status_t status = network_device_.Initialize(GetParentNode(), netdev_dispatcher_.get(),
                                                  outgoing, kNetDevDriverName);
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

  // If we are operating with manufacturing firmware ensure SoftAP IF is also not present
  if (brcmf_feat_is_enabled(brcmf_pub_.get(), BRCMF_FEAT_MFG)) {
    if (ap_interface_ || client_interface_) {
      // Either the interface we're trying to create exists or the other one exists. Neither is
      // supported in manufacturing FW.
      BRCMF_ERR("Simultaneous mode not supported in mfg FW - IF already exists");
      completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
      return;
    }
  }

  std::unique_ptr<WlanInterface>* interface = GetInterfaceForRole(request->role());
  if (!interface) {
    BRCMF_ERR("MAC role %u not supported", fidl::ToUnderlying(request->role()));
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  InterfaceInfo info = GetInterfaceInfoForRole(request->role());
  if (*interface) {
    BRCMF_ERR("Device::CreateIface() %s interface already exists", info.display_name);
    completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
    return;
  }

  wireless_dev* wdev = nullptr;
  const zx_status_t status =
      brcmf_cfg80211_add_iface(brcmf_pub_.get(), info.interface_name, nullptr, request, &wdev);
  if (status != ZX_OK) {
    BRCMF_ERR("Device::CreateIface() failed to create %s interface, %s", info.display_name,
              zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  WlanInterface::Create(
      this, info.interface_name, wdev, request->role(), info.interface_id,
      [info, interface, wdev, this,
       completer = completer.ToAsync()](zx::result<std::unique_ptr<WlanInterface>> result) mutable {
        fdf::Arena arena('WLIF');
        if (result.is_error()) {
          BRCMF_ERR("Failed to create WlanInterface: %s", result.status_string());
          completer.buffer(arena).ReplyError(result.status_value());
          return;
        }
        {
          // Hold the lock while modifying iface_ptr which points to a member of Device.
          std::lock_guard<std::mutex> lock(lock_);
          *interface = std::move(result.value());
        }

        net_device* ndev = wdev->netdev;
        BRCMF_DBG(WLANPHY, "Created %s iface with netdev:%s id:%d", info.display_name, ndev->name,
                  info.interface_id);
#if !defined(NDEBUG)
        const uint8_t* mac_addr = ndev_to_if(ndev)->mac_addr;
        BRCMF_DBG(WLANPHY, "  address: " FMT_MAC, FMT_MAC_ARGS(mac_addr));
#endif /* !defined(NDEBUG) */

        completer.buffer(arena).ReplySuccess(
            fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(arena)
                .iface_id(info.interface_id)
                .Build());
      });
}

void Device::DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                          DestroyIfaceCompleter::Sync& completer) {
  if (!request->has_iface_id()) {
    BRCMF_ERR("Device::DestroyIface() invoked without valid iface_id");
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  DestroyIface(request->iface_id(), [completer = completer.ToAsync()](zx_status_t status) mutable {
    fdf::Arena arena('BRCM');
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
    } else {
      completer.buffer(arena).ReplySuccess();
    }
  });
}

void Device::DestroyIface(uint16_t iface_id, fit::callback<void(zx_status_t)>&& on_complete) {
  std::lock_guard<std::mutex> lock(lock_);

  std::unique_ptr<WlanInterface>* iface_ptr = GetInterfaceForId(iface_id);
  if (!iface_ptr) {
    BRCMF_ERR("Device::DestroyIface() Unknown interface id: %d", iface_id);
    on_complete(ZX_ERR_NOT_FOUND);
    return;
  }
  InterfaceInfo info = GetInterfaceInfoForRole(GetMacRoleForInterfaceId(iface_id));

  WlanInterface* iface = iface_ptr->get();
  if (!iface) {
    // Check the pointer inside the pointer, the actual interface pointer.
    BRCMF_WARN("%s interface not found", info.display_name);
    on_complete(ZX_ERR_NOT_FOUND);
    return;
  }

  BRCMF_DBG(WLANPHY, "Destroying %s interface", info.display_name);
  iface->DestroyIface([iface_ptr, iface_id, info, this,
                       on_complete = std::move(on_complete)](zx_status_t status) mutable {
    fdf::Arena arena('BRCM');
    if (status != ZX_OK) {
      // Don't reset iface_ptr here since we failed to delete it.
      BRCMF_ERR("Device::DestroyIface() Error destroying %s interface : %s", info.display_name,
                zx_status_get_string(status));
      on_complete(status);
      return;
    }
    BRCMF_DBG(WLANPHY, "%s interface %u destroyed successfully", info.display_name, iface_id);
    {
      // Hold the lock while modifying iface_ptr which points to a member of Device.
      std::lock_guard<std::mutex> lock(lock_);
      iface_ptr->reset();
    }
    on_complete(ZX_OK);
  });
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
  // This will be called as the final step of the network device removal. We can now call the
  // shutdown handler to indicate that everything was shut down properly.
  if (on_netdev_shutdown_complete_) {
    on_netdev_shutdown_complete_();
  }
}

void Device::NetDevStart(wlan::drivers::components::NetworkDevice::Callbacks::StartTxn txn) {
  txn.Reply(ZX_OK);
}

void Device::NetDevStop(wlan::drivers::components::NetworkDevice::Callbacks::StopTxn txn) {
  // Flush all buffers in response to this call. They are no longer valid for use.
  brcmf_flush_buffers(drvr());
  txn.Reply();
}

void Device::NetDevGetInfo(fuchsia_hardware_network_driver::DeviceImplInfo* out_info) {
  std::lock_guard<std::mutex> lock(lock_);

  uint16_t tx_depth = 0;
  zx_status_t err = brcmf_get_tx_depth(drvr(), &tx_depth);
  ZX_ASSERT(err == ZX_OK);
  uint16_t rx_depth = 0;
  err = brcmf_get_rx_depth(drvr(), &rx_depth);
  ZX_ASSERT(err == ZX_OK);
  uint16_t tx_tail_length = 0;
  brcmf_get_tail_length(drvr(), &tx_tail_length);

  out_info->tx_depth() = tx_depth;
  out_info->rx_depth() = rx_depth;
  out_info->rx_threshold() = rx_depth / 3;
  out_info->max_buffer_parts() = kMaxBufferParts;
  out_info->max_buffer_length() = ZX_PAGE_SIZE;
  out_info->buffer_alignment() = ZX_PAGE_SIZE;
  out_info->min_rx_buffer_length() = IEEE80211_MSDU_SIZE_MAX;
  out_info->tx_head_length() = drvr()->hdrlen;
  out_info->tx_tail_length() = tx_tail_length;
  // No hardware acceleration supported yet.
  out_info->rx_accel() = std::nullopt;
  out_info->tx_accel() = std::nullopt;
}

void Device::NetDevQueueTx(cpp20::span<wlan::drivers::components::Frame> frames) {
  brcmf_start_xmit(drvr(), frames);
}

void Device::NetDevQueueRxSpace(
    cpp20::span<const fuchsia_hardware_network_driver::wire::RxSpaceBuffer> buffers,
    uint8_t* vmo_addrs[]) {
  brcmf_queue_rx_space(drvr(), buffers, vmo_addrs);
}

zx_status_t Device::NetDevPrepareVmo(uint8_t vmo_id, zx::vmo vmo, uint8_t* mapped_address,
                                     size_t mapped_size) {
  return brcmf_prepare_vmo(drvr(), vmo_id, vmo.get(), mapped_address, mapped_size);
}

void Device::NetDevReleaseVmo(uint8_t vmo_id) { brcmf_release_vmo(drvr(), vmo_id); }

void Device::NetDevSetSnoopEnabled(bool snoop) {}

void Device::DestroyAllIfaces(fit::callback<void()>&& on_complete) {
  std::lock_guard<std::mutex> lock(lock_);

  // Pick an interface to start destroying. By moving the pointer we ensure that the next recursive
  // call to DestroyAllIfaces will not pick up that pointer again.
  std::unique_ptr<WlanInterface> interface =
      client_interface_ ? std::move(client_interface_) : std::move(ap_interface_);

  if (!interface) {
    // No interfaces left to destroy, call on_complete
    on_complete();
    return;
  }

  // Capture interface to keep it alive until destruction completes. Use a raw pointer to safely
  // call into it after moving it.
  WlanInterface* interface_ptr = interface.get();
  interface_ptr->DestroyIface([this, interface = std::move(interface),
                               on_complete = std::move(on_complete)](zx_status_t status) mutable {
    if (status != ZX_OK) {
      InterfaceInfo info = GetInterfaceInfoForRole(interface->Role());
      BRCMF_ERR("Device::DestroyAllIfaces() : Failed to destroy %s interface : %s",
                info.display_name, zx_status_get_string(status));
    }
    // The interface here is moved out of the Device object, it can no longer be accessed by any
    // other code so no locking is needed here.
    interface.reset();

    // Call DestroyAllIfaces again to destroy the next interface, if any.
    DestroyAllIfaces(std::move(on_complete));
  });
}

std::unique_ptr<WlanInterface>* Device::GetInterfaceForRole(
    fuchsia_wlan_common::wire::WlanMacRole role) {
  switch (role) {
    case fuchsia_wlan_common::wire::WlanMacRole::kClient:
      return &client_interface_;
    case fuchsia_wlan_common::wire::WlanMacRole::kAp:
      return &ap_interface_;
    default:
      return nullptr;
  }
}

std::unique_ptr<WlanInterface>* Device::GetInterfaceForId(uint16_t interface_id) {
  switch (interface_id) {
    case kClientInterfaceId:
      return &client_interface_;
    case kApInterfaceId:
      return &ap_interface_;
    default:
      return nullptr;
  }
}

}  // namespace brcmfmac
}  // namespace wlan
