// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi.h"

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/fit/function.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/spi/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "spi-child.h"

namespace spi {

zx::result<> SpiDriver::Start() {
  zx::result metadata_result =
      fdf_metadata::GetMetadata<fuchsia_hardware_spi_businfo::SpiBusMetadata>(*incoming());
  if (metadata_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get SPI metadata: %s", metadata_result.status_string());
    return metadata_result.take_error();
  }
  fuchsia_hardware_spi_businfo::SpiBusMetadata& metadata = metadata_result.value();

  if (!metadata.bus_id()) {
    FDF_LOG(ERROR, "No bus ID metadata provided");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  bus_id_ = *metadata.bus_id();

  zx::result scheduler_role_name_result =
      fdf_metadata::GetMetadataIfExists<fuchsia_scheduler::RoleName>(*incoming());
  if (scheduler_role_name_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get scheduler role name: %s",
            scheduler_role_name_result.status_string());
    return scheduler_role_name_result.take_error();
  }
  if (scheduler_role_name_result.value().has_value()) {
    const auto& scheduler_role_name = scheduler_role_name_result.value().value();

    zx::result result = fdf::SynchronizedDispatcher::Create(
        {}, "SPI", [](fdf_dispatcher_t*) {}, scheduler_role_name.role());
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create SynchronizedDispatcher: %s", result.status_string());
      return result.take_error();
    }

    // If scheduler role metadata was provided, create a new dispatcher using the role, and use that
    // dispatcher instead of the default dispatcher passed to this method.
    fidl_dispatcher_.emplace(*std::move(result));

    FDF_LOG(DEBUG, "Using dispatcher with role \"%s\"", scheduler_role_name.role().c_str());
  }

  zx::result spi_impl_client_end = incoming()->Connect<fuchsia_hardware_spiimpl::Service::Device>();
  if (spi_impl_client_end.is_error()) {
    return spi_impl_client_end.take_error();
  }

  fdf::WireSharedClient spi_impl(*std::move(spi_impl_client_end), fidl_dispatcher()->get());

  zx::result child = AddOwnedChild(kChildNodeName);
  if (child.is_error()) {
    FDF_LOG(ERROR, "Failed to add owned child: %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(child.value());

  if (metadata.channels()) {
    if (zx::result result = AddChildren(metadata, std::move(spi_impl)); result.is_error()) {
      return result.take_error();
    }
  } else {
    FDF_LOG(INFO, "No channels supplied.");
  }

  return zx::ok();
}

zx::result<> SpiDriver::AddChildren(
    const fuchsia_hardware_spi_businfo::SpiBusMetadata& metadata,
    fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> client) {
  bool has_siblings = metadata.channels()->size() > 1;
  for (auto& channel : *metadata.channels()) {
    const auto cs = channel.cs().value_or(0);
    const auto vid = channel.vid().value_or(0);
    const auto pid = channel.pid().value_or(0);
    const auto did = channel.did().value_or(0);

    char name[20];
    snprintf(name, sizeof(name), "spi-%u-%u", bus_id_, cs);

    std::unique_ptr compat_server = std::make_unique<compat::SyncInitializedDeviceServer>();

    {
      auto result = compat_server->Initialize(incoming(), outgoing(), node_name(), name);
      if (result.is_error()) {
        FDF_LOG(ERROR, "Failed to initialize compat server: %s", result.status_string());
        return result.take_error();
      }
    }

    fidl::Arena arena;

    std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server->CreateOffers2(arena);
    offers.push_back(fdf::MakeOffer2<fuchsia_hardware_spi::Service>(arena, name));

    auto [controller_client, controller_server] =
        fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

    fbl::AllocChecker ac;

    std::unique_ptr<SpiChild> dev(new (&ac) SpiChild(client.Clone(), cs, has_siblings,
                                                     fidl_dispatcher(), std::move(compat_server),
                                                     std::move(controller_client)));

    if (!ac.check()) {
      FDF_LOG(ERROR, "Out of memory");
      return zx::error(ZX_ERR_NO_MEMORY);
    }

    auto serve_result =
        outgoing()->AddService<fuchsia_hardware_spi::Service>(dev->CreateInstanceHandler(), name);
    if (serve_result.is_error()) {
      FDF_LOG(ERROR, "Failed to add SPI service: %s", serve_result.status_string());
      return serve_result.take_error();
    }

    std::vector<fuchsia_driver_framework::wire::NodeProperty> props{
        fdf::MakeProperty(arena, bind_fuchsia::SPI_BUS_ID, bus_id_),
        fdf::MakeProperty(arena, bind_fuchsia::SPI_CHIP_SELECT, cs),
    };
    if (vid || pid || did) {
      props.push_back(fdf::MakeProperty(arena, bind_fuchsia::PLATFORM_DEV_VID, vid));
      props.push_back(fdf::MakeProperty(arena, bind_fuchsia::PLATFORM_DEV_PID, pid));
      props.push_back(fdf::MakeProperty(arena, bind_fuchsia::PLATFORM_DEV_DID, did));
    }

    auto connector = dev->BindDevfs();
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                     .connector(*std::move(connector))
                     .connector_supports(fuchsia_device_fs::ConnectionType::kDevice)
                     .class_name("spi")
                     .Build();

    const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                          .name(arena, name)
                          .offers2(offers)
                          .properties(props)
                          .devfs_args(devfs)
                          .Build();

    auto result = fidl::WireCall(child_.node_)->AddChild(args, std::move(controller_server), {});
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add SPI child node: %s",
              result.error().FormatDescription().c_str());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to add SPI child node");
      return zx::error(ZX_ERR_INTERNAL);
    }

    children_.push_back(std::move(dev));
  }

  return zx::ok();
}

}  // namespace spi

FUCHSIA_DRIVER_EXPORT(spi::SpiDriver);
