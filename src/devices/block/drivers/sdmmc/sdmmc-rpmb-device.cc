// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-rpmb-device.h"

#include <lib/fdf/dispatcher.h>

#include <bind/fuchsia/hardware/rpmb/cpp/bind.h>

#include "sdmmc-block-device.h"
#include "sdmmc-root-device.h"
#include "sdmmc-types.h"

namespace sdmmc {

zx_status_t RpmbDevice::AddDevice() {
  {
    const std::string path_from_parent = std::string(sdmmc_parent_->parent()->driver_name()) + "/" +
                                         std::string(sdmmc_parent_->block_name()) + "/";
    auto result = compat_server_.Initialize(
        sdmmc_parent_->parent()->driver_incoming(), sdmmc_parent_->parent()->driver_outgoing(),
        sdmmc_parent_->parent()->driver_node_name(), kDeviceName, compat::ForwardMetadata::None(),
        std::nullopt, path_from_parent);
    if (result.is_error()) {
      return result.status_value();
    }
  }

  {
    fuchsia_hardware_rpmb::Service::InstanceHandler handler({
        .device = fit::bind_member<&RpmbDevice::Serve>(this),
    });
    auto result =
        sdmmc_parent_->parent()->driver_outgoing()->AddService<fuchsia_hardware_rpmb::Service>(
            std::move(handler));
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add RPMB service: %s", result.status_string());
      return result.status_value();
    }
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to create controller endpoints: %s",
             controller_endpoints.status_string());
    return controller_endpoints.status_value();
  }

  controller_.Bind(std::move(controller_endpoints->client));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_hardware_rpmb::SERVICE,
                                    bind_fuchsia_hardware_rpmb::SERVICE_ZIRCONTRANSPORT);

  std::vector<fuchsia_component_decl::wire::Offer> offers = compat_server_.CreateOffers(arena);
  offers.push_back(
      fdf::MakeOffer<fuchsia_hardware_rpmb::Service>(arena, component::kDefaultInstance));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, kDeviceName)
                        .offers(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result =
      sdmmc_parent_->block_node()->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child partition device: %s", result.status_string());
    return result.status();
  }
  return ZX_OK;
}

void RpmbDevice::Serve(fidl::ServerEnd<fuchsia_hardware_rpmb::Rpmb> request) {
  fidl::BindServer(sdmmc_parent_->parent()->driver_async_dispatcher(), std::move(request), this);
}

void RpmbDevice::GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) {
  using DeviceInfo = fuchsia_hardware_rpmb::wire::DeviceInfo;
  using EmmcDeviceInfo = fuchsia_hardware_rpmb::wire::EmmcDeviceInfo;

  EmmcDeviceInfo emmc_info = {};
  memcpy(emmc_info.cid.data(), cid_.data(), cid_.size() * sizeof(cid_[0]));
  emmc_info.rpmb_size = rpmb_size_;
  emmc_info.reliable_write_sector_count = reliable_write_sector_count_;

  auto emmc_info_ptr = fidl::ObjectView<EmmcDeviceInfo>::FromExternal(&emmc_info);

  completer.ToAsync().Reply(DeviceInfo::WithEmmcInfo(emmc_info_ptr));
}

void RpmbDevice::Request(RequestRequestView request, RequestCompleter::Sync& completer) {
  RpmbRequestInfo info = {
      .tx_frames = std::move(request->request.tx_frames),
      .completer = completer.ToAsync(),
  };

  if (request->request.rx_frames) {
    info.rx_frames = {
        .vmo = std::move(request->request.rx_frames->vmo),
        .offset = request->request.rx_frames->offset,
        .size = request->request.rx_frames->size,
    };
  }

  sdmmc_parent_->RpmbQueue(std::move(info));
}

fdf::Logger& RpmbDevice::logger() { return sdmmc_parent_->logger(); }

}  // namespace sdmmc
