// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-phy-impl.h"

#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fidl/cpp/wire/status.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <utility>

#include <wlan/common/phy.h>

#include "utils.h"

namespace wlan {

std::shared_ptr<WlanPhyImplDevice> WlanPhyImplDevice::New(
    const std::shared_ptr<const WlantapDriverContext>& context, zx::channel user_channel,
    const std::shared_ptr<const wlan_tap::WlantapPhyConfig>& phy_config,
    NodeControllerClient phy_controller) {
  auto device = std::shared_ptr<WlanPhyImplDevice>(new WlanPhyImplDevice(context, phy_config));
  device->Init(std::move(user_channel), std::move(phy_controller));
  return device;
}

WlanPhyImplDevice::WlanPhyImplDevice(
    const std::shared_ptr<const WlantapDriverContext>& context,
    const std::shared_ptr<const wlan_tap::WlantapPhyConfig>& phy_config)
    : driver_context_(context), phy_config_(phy_config) {}

void WlanPhyImplDevice::Init(
    zx::channel user_channel,
    fidl::ClientEnd<fuchsia_driver_framework::NodeController> phy_controller) {
  wlantap_phy_ = std::make_unique<WlantapPhy>(
      std::move(user_channel), phy_config_,
      [self = shared_from_this(),
       name = name_](WlantapPhy::ShutdownCompleter::Async wlantap_phy_shutdown_completer) mutable
      -> fit::result<zx_status_t> {
        // Return an error if |self| has already been reset(). This function
        // should only be called once, and |self| is reset() upon completion
        // to drop its reference.
        if (self == nullptr) {
          FDF_LOG(ERROR, "%s: shutdown callback called more than once", name.c_str());
          return fit::error(ZX_ERR_INTERNAL);
        }

        self->wlantap_phy_shutdown_completer_ = std::move(wlantap_phy_shutdown_completer);

        // Unbind the WlanPhyImpl child node. This effectively blocks iface
        // management from the outside.
        auto phy_removal_status = self->phy_controller_->Remove();
        fit::result<zx_status_t> result = fit::ok();
        if (phy_removal_status.is_error()) {
          FDF_LOG(ERROR, "%s: Could not remove phy: %s", name.c_str(),
                  phy_removal_status.error_value().status_string());
          self->ShutdownComplete();

          result = fit::error(phy_removal_status.error_value().status());
        }
        self.reset();
        return result;
      });

  // The PhyControllerEventHandler class detects when NodeController server associated
  // with the phy is no longer available. This normally occurs during shutdown.
  class PhyControllerEventHandler
      : public fidl::AsyncEventHandler<fuchsia_driver_framework::NodeController> {
   public:
    explicit PhyControllerEventHandler(std::shared_ptr<WlanPhyImplDevice> device)
        : device_(std::move(device)) {}
    void on_fidl_error(::fidl::UnbindInfo error) override {
      // TODO(b/307809104): Determine which FIDL errors are
      // normal during shutdown.
      FDF_LOG(INFO, "%s: phy node unbound: %s", device_->name_.c_str(),
              error.FormatDescription().c_str());

      // If an iface exists, then iface destruction will call ShutdownComplete().
      if (device_->IfaceExists()) {
        auto status = device_->iface_controller_->Remove();
        if (status.is_error()) {
          FDF_LOG(ERROR, "%s: Could not remove iface: %s", device_->name_.c_str(),
                  status.error_value().status_string());
          device_->ShutdownComplete();
        }
      } else {
        device_->ShutdownComplete();
      }
      delete this;
    }
    void handle_unknown_event(
        fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

   private:
    std::shared_ptr<WlanPhyImplDevice> device_;
  };
  phy_controller_.Bind(std::move(phy_controller), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                       new PhyControllerEventHandler(shared_from_this()));
}

void WlanPhyImplDevice::GetSupportedMacRoles(fdf::Arena& arena,
                                             GetSupportedMacRolesCompleter::Sync& completer) {
  // wlantap-phy only supports a single mac role determined by the config
  wlan_common::WlanMacRole supported[1] = {phy_config_->mac_role};
  auto reply_vec = fidl::VectorView<wlan_common::WlanMacRole>::FromExternal(supported, 1);

  FDF_LOG(INFO, "%s: received a 'GetSupportedMacRoles' DDK request. Responding with roles = {%u}",
          name_.c_str(), static_cast<uint32_t>(phy_config_->mac_role));

  auto response =
      fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(arena)
          .supported_mac_roles(reply_vec)
          .Build();
  completer.buffer(arena).ReplySuccess(response);
}

void WlanPhyImplDevice::CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                                    CreateIfaceCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: received a 'CreateIface' request", name_.c_str());

  std::string role_str = RoleToString(request->role());
  FDF_LOG(INFO, "%s: received a 'CreateIface' for role: %s", name_.c_str(), role_str.c_str());
  if (phy_config_->mac_role != request->role()) {
    FDF_LOG(ERROR, "%s: CreateIface(%s): role not supported", name_.c_str(), role_str.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (IfaceExists()) {
    FDF_LOG(ERROR,
            "%s: CreateIface(%s): Failed to create iface. wlantap only supports at most one iface.",
            name_.c_str(), role_str.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }

  if (!request->mlme_channel().is_valid()) {
    FDF_LOG(ERROR, "%s: CreateIface(%s): MLME channel in request is invalid", name_.c_str(),
            role_str.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_IO_INVALID);
    return;
  }

  zx_status_t status = CreateWlanSoftmac(request->role(), std::move(request->mlme_channel()));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: CreateIface(%s): Could not create softmac: %s", name_.c_str(),
            role_str.c_str(), zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  fidl::Arena fidl_arena;
  auto resp = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena)
                  .iface_id(0)
                  .Build();
  completer.buffer(arena).ReplySuccess(resp);
}

bool WlanPhyImplDevice::IfaceExists() {
  return this->iface_controller_.is_valid() && this->wlantap_mac_;
}

fit::result<zx_status_t> WlanPhyImplDevice::DestroyIface() {
  if (!IfaceExists()) {
    FDF_LOG(ERROR, "%s: Iface doesn't exist", name_.c_str());
    return fit::error(ZX_ERR_NOT_FOUND);
  }

  auto status = iface_controller_->Remove();
  if (status.is_error()) {
    FDF_LOG(ERROR, "%s: Failed to destroy iface: %s", name_.c_str(),
            status.error_value().status_string());
    return fit::error(status.error_value().status());
  }

  wlantap_mac_.reset();

  return fit::ok();
}

// Calls the stored ShutdownCompleter received through WlantapPhy.Shutdown().
void WlanPhyImplDevice::ShutdownComplete() {
  if (this->wlantap_phy_shutdown_completer_.has_value()) {
    this->wlantap_phy_shutdown_completer_->Reply();
  }
}

void WlanPhyImplDevice::DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                                     DestroyIfaceCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: received a 'DestroyIface' DDK request", name_.c_str());
  completer.buffer(arena).Reply(DestroyIface());
}

void WlanPhyImplDevice::SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                                   SetCountryCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: SetCountry() to [%s] received", name_.c_str(),
          wlan::common::Alpha2ToStr(request->alpha2()).c_str());

  wlan_tap::SetCountryArgs args{.alpha2 = request->alpha2()};
  zx_status_t status = wlantap_phy_->SetCountry(args);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: SetCountry() failed: %s", name_.c_str(), zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
}

void WlanPhyImplDevice::ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: ClearCountry() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: GetCountry() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                                         SetPowerSaveModeCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: SetPowerSaveMode() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void WlanPhyImplDevice::GetPowerSaveMode(fdf::Arena& arena,
                                         GetPowerSaveModeCompleter::Sync& completer) {
  FDF_LOG(WARNING, "%s: GetPowerSaveMode() not supported", name_.c_str());
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t WlanPhyImplDevice::CreateWlanSoftmac(wlan_common::WlanMacRole role,
                                                 zx::channel mlme_channel) {
  static size_t n = 0;
  char name[ZX_MAX_NAME_LEN + 1];
  snprintf(name, sizeof(name), "wlansoftmac-%lu", n++);

  auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "%s: Failed to create endpoints: %s", name_.c_str(), endpoints.status_string());
    return endpoints.status_value();
  }

  zx_status_t status = ServeWlanSoftmac(name, role, std::move(mlme_channel));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: ServeWlanSoftmac failed: %s", name_.c_str(), zx_status_get_string(status));
    return status;
  }

  status = AddWlanSoftmacChild(name, std::move(endpoints->server));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: AddWlanSoftmacChild failed: %s", name_.c_str(),
            zx_status_get_string(status));
    return status;
  }

  // The IfaceControllerEventHandler class detects when NodeController server associated
  // with an iface is no longer available. This normally occurs during shutdown.
  class IfaceControllerEventHandler
      : public fidl::AsyncEventHandler<fuchsia_driver_framework::NodeController> {
   public:
    explicit IfaceControllerEventHandler(std::shared_ptr<WlanPhyImplDevice> device)
        : device_(std::move(device)) {}
    void on_fidl_error(::fidl::UnbindInfo error) override {
      // TODO(b/307809104): Determine which FIDL errors are
      // normal during shutdown and iface destruction.
      FDF_LOG(INFO, "%s: Iface node unbound: %s", device_->name_.c_str(),
              error.FormatDescription().c_str());
      if (device_->wlantap_phy_shutdown_completer_.has_value()) {
        device_->ShutdownComplete();
      }
      delete this;
    }
    void handle_unknown_event(
        fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

   private:
    std::shared_ptr<WlanPhyImplDevice> device_;
  };
  iface_controller_.Bind(std::move(endpoints->client),
                         fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                         new IfaceControllerEventHandler(shared_from_this()));
  return ZX_OK;
}

zx_status_t WlanPhyImplDevice::AddWlanSoftmacChild(
    std::string_view name, fidl::ServerEnd<fuchsia_driver_framework::NodeController> server) {
  fidl::Arena arena;

  auto offers = std::vector{fdf::MakeOffer2<fuchsia_wlan_softmac::Service>(arena, name)};
  FDF_LOG(INFO, "%s: Creating Child node", name_.c_str());
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(name)
                  .offers2(offers)
                  .Build();

  auto res = driver_context_->node_client()->AddChild(args, std::move(server), {});
  if (!res.ok()) {
    FDF_LOG(ERROR, "%s: Failed to add child: %s", name_.c_str(), res.status_string());
    return res.status();
  }

  return ZX_OK;
}

zx_status_t WlanPhyImplDevice::ServeWlanSoftmac(std::string_view name,
                                                wlan_common::WlanMacRole role,
                                                zx::channel mlme_channel) {
  if (wlantap_mac_) {
    FDF_LOG(ERROR, "%s: Softmac already exists, only one allowed", name_.c_str());
    return ZX_ERR_ALREADY_EXISTS;
  }

  wlantap_mac_ =
      std::make_unique<WlantapMac>(wlantap_phy_.get(), role, phy_config_, std::move(mlme_channel));

  FDF_LOG(INFO, "%s: Adding softmac outgoing service", name_.c_str());
  fuchsia_wlan_softmac::Service::InstanceHandler handler(
      {.wlan_softmac = wlantap_mac_->ProtocolHandler()});

  zx::result result = driver_context_->outgoing()->AddService<fuchsia_wlan_softmac::Service>(
      std::move(handler), name);

  if (result.is_error()) {
    FDF_LOG(ERROR, "%s: Failed To add WlanSoftmac service: %s", name_.c_str(),
            result.status_string());
    return result.status_value();
  }

  return ZX_OK;
}

}  // namespace wlan
