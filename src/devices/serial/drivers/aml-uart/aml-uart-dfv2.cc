// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart-dfv2.h"

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/serial/cpp/bind.h>

namespace serial {

namespace {

constexpr std::string_view pdev_name = "pdev";
constexpr std::string_view child_name = "aml-uart";
constexpr std::string_view driver_name = "aml-uart";

}  // namespace

AmlUartV2::AmlUartV2(fdf::DriverStartArgs start_args,
                     fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase(driver_name, std::move(start_args), std::move(driver_dispatcher)),
      driver_config_(take_config<aml_uart_dfv2_config::Config>()) {}

void AmlUartV2::Start(fdf::StartCompleter completer) {
  start_completer_.emplace(std::move(completer));

  parent_node_client_.Bind(std::move(node()), dispatcher());

  // pdev is our primary node so that is what this will be connecting to for the compat connection.
  auto compat_connection = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
  if (compat_connection.is_error()) {
    CompleteStart(compat_connection.take_error());
    return;
  }
  compat_client_.Bind(std::move(compat_connection.value()), dispatcher());
  compat_client_->GetMetadata().Then(fit::bind_member<&AmlUartV2::OnReceivedMetadata>(this));
}

void AmlUartV2::PrepareStop(fdf::PrepareStopCompleter completer) {
  if (aml_uart_.has_value()) {
    aml_uart_->Enable(false);
  }

  if (irq_dispatcher_.has_value()) {
    // The shutdown is async. When it is done, the dispatcher's shutdown callback will complete
    // the PrepareStopCompleter.
    prepare_stop_completer_.emplace(std::move(completer));
    irq_dispatcher_->ShutdownAsync();
  } else {
    // No irq_dispatcher_, just reply to the PrepareStopCompleter, timer_dispatcher_ is created
    // after irq_dispatcher_, so we assume that there's no timer_dispatcher_ either.
    completer(zx::ok());
  }
}

AmlUart& AmlUartV2::aml_uart_for_testing() {
  ZX_ASSERT(aml_uart_.has_value());
  return aml_uart_.value();
}

void AmlUartV2::OnReceivedMetadata(
    fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& metadata_result) {
  if (!metadata_result.ok()) {
    FDF_LOG(ERROR, "Failed to get metadata %s", metadata_result.status_string());
    CompleteStart(zx::error(metadata_result.status()));
    return;
  }

  if (metadata_result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to get metadata %s",
            zx_status_get_string(metadata_result.value().error_value()));
    CompleteStart(zx::error(metadata_result.value().error_value()));
    return;
  }

  for (auto& metadata : metadata_result->value()->metadata) {
    if (metadata.type == DEVICE_METADATA_SERIAL_PORT_INFO) {
      size_t size;
      zx_status_t status =
          metadata.data.get_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get metadata vmo size: %s", zx_status_get_string(status));
        CompleteStart(zx::error(status));
        return;
      }

      std::vector<uint8_t> fidl_info_buffer(size);
      status = metadata.data.read(fidl_info_buffer.data(), 0, fidl_info_buffer.size());
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read metadata vmo: %s", zx_status_get_string(status));
        CompleteStart(zx::error(status));
        return;
      }

      fit::result fidl_info =
          fidl::InplaceUnpersist<fuchsia_hardware_serial::wire::SerialPortInfo>(fidl_info_buffer);
      if (fidl_info.is_error()) {
        FDF_LOG(ERROR, "Failed to decode metadata: %s",
                fidl_info.error_value().FormatDescription().c_str());
        CompleteStart(zx::error(fidl_info.error_value().status()));
        return;
      }

      serial_port_info_ = {
          .serial_class = fidl_info->serial_class,
          .serial_vid = fidl_info->serial_vid,
          .serial_pid = fidl_info->serial_pid,
      };

      // We can break since we have now read DEVICE_METADATA_SERIAL_PORT_INFO.
      break;
    }
  }

  device_server_.Begin(incoming(), outgoing(), node_name(), child_name,
                       fit::bind_member<&AmlUartV2::OnDeviceServerInitialized>(this),
                       compat::ForwardMetadata::Some({DEVICE_METADATA_MAC_ADDRESS}));
}

zx_status_t AmlUartV2::GetPowerConfiguration(
    const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev) {
  auto sag = incoming()->Connect<fuchsia_power_system::ActivityGovernor>();
  if (sag.is_error() || !sag->is_valid()) {
    FDF_LOG(WARNING, "Failed to connect to activity governor: %s", sag.status_string());
    return sag.status_value();
  }

  sag_ = std::move(sag.value());

  return ZX_OK;
}

void AmlUartV2::OnDeviceServerInitialized(zx::result<> device_server_init_result) {
  if (device_server_init_result.is_error()) {
    CompleteStart(device_server_init_result.take_error());
    return;
  }

  auto pdev_connection =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>(pdev_name);
  if (pdev_connection.is_error()) {
    CompleteStart(pdev_connection.take_error());
    return;
  }

  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> fidl_pdev(
      std::move(pdev_connection.value()));

  if (driver_config_.enable_suspend()) {
    if (zx_status_t status = GetPowerConfiguration(fidl_pdev); status != ZX_OK) {
      FDF_LOG(INFO, "Could not get power configuration: %s", zx_status_get_string(status));
      CompleteStart(zx::error(status));
      return;
    }
  }

  fdf::PDev pdev(fidl_pdev.TakeClientEnd());

  zx::result mmio = pdev.MapMmio(0);
  if (mmio.is_error()) {
    FDF_SLOG(ERROR, "Failed to map mmio.", KV("status", mmio.status_string()));
    CompleteStart(mmio.take_error());
    return;
  }

  zx::result irq_dispatcher_result =
      fdf::SynchronizedDispatcher::Create({}, "aml_uart_irq", [this](fdf_dispatcher_t*) {
        // Shutdown timer dispatcher if there is one, otherwise call prepare_stop_completer_.
        if (timer_dispatcher_) {
          timer_dispatcher_->ShutdownAsync();
          return;
        }
        if (prepare_stop_completer_.has_value()) {
          fdf::PrepareStopCompleter completer = std::move(prepare_stop_completer_.value());
          prepare_stop_completer_.reset();
          completer(zx::ok());
        }
      });
  if (irq_dispatcher_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create irq dispatcher: %s", irq_dispatcher_result.status_string());
    CompleteStart(irq_dispatcher_result.take_error());
    return;
  }

  irq_dispatcher_.emplace(std::move(irq_dispatcher_result.value()));

  zx::result timer_dispatcher_result =
      fdf::SynchronizedDispatcher::Create({}, "aml_uart_timer", [this](fdf_dispatcher_t*) {
        if (prepare_stop_completer_.has_value()) {
          fdf::PrepareStopCompleter completer = std::move(prepare_stop_completer_.value());
          prepare_stop_completer_.reset();
          completer(zx::ok());
        }
      });
  if (timer_dispatcher_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create timer dispatcher: %s",
            timer_dispatcher_result.status_string());
    CompleteStart(timer_dispatcher_result.take_error());
    return;
  }

  timer_dispatcher_.emplace(std::move(timer_dispatcher_result.value()));
  aml_uart_.emplace(std::move(pdev), serial_port_info_, std::move(mmio.value()),
                    irq_dispatcher_->borrow(), timer_dispatcher_->borrow(),
                    driver_config_.enable_suspend(), std::move(sag_));

  // Default configuration for the case that serial_impl_config is not called.
  constexpr uint32_t kDefaultBaudRate = 115200;
  constexpr uint32_t kDefaultConfig = fuchsia_hardware_serialimpl::kSerialDataBits8 |
                                      fuchsia_hardware_serialimpl::kSerialStopBits1 |
                                      fuchsia_hardware_serialimpl::kSerialParityNone;
  aml_uart_->Config(kDefaultBaudRate, kDefaultConfig);

  zx::result node_controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (node_controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create NodeController endpoints %s",
            node_controller_endpoints.status_string());
    CompleteStart(node_controller_endpoints.take_error());
    return;
  }

  fuchsia_hardware_serialimpl::Service::InstanceHandler handler({
      .device =
          [this](fdf::ServerEnd<fuchsia_hardware_serialimpl::Device> server_end) {
            serial_impl_bindings_.AddBinding(driver_dispatcher()->get(), std::move(server_end),
                                             &aml_uart_.value(), fidl::kIgnoreBindingClosure);
          },
  });
  zx::result<> add_result =
      outgoing()->AddService<fuchsia_hardware_serialimpl::Service>(std::move(handler), child_name);
  if (add_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add fuchsia_hardware_serialimpl::Service %s",
            add_result.status_string());
    CompleteStart(add_result.take_error());
    return;
  }

  auto offers = device_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_serialimpl::Service>(child_name));

  fuchsia_driver_framework::NodeAddArgs args{
      {
          .name = std::string(child_name),
          .properties = {{
              fdf::MakeProperty(bind_fuchsia::PROTOCOL,
                                bind_fuchsia_serial::BIND_PROTOCOL_IMPL_ASYNC),
              fdf::MakeProperty(bind_fuchsia::SERIAL_CLASS,
                                static_cast<uint32_t>(aml_uart_->serial_port_info().serial_class)),
          }},
          .offers2 = std::move(offers),
      },
  };

  fidl::Arena arena;
  parent_node_client_
      ->AddChild(fidl::ToWire(arena, std::move(args)), std::move(node_controller_endpoints->server),
                 {})
      .Then(fit::bind_member<&AmlUartV2::OnAddChildResult>(this));
}

void AmlUartV2::OnAddChildResult(
    fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>& add_child_result) {
  if (!add_child_result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", add_child_result.status_string());
    CompleteStart(zx::error(add_child_result.status()));
    return;
  }

  if (add_child_result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to add child. NodeError: %d",
            static_cast<uint32_t>(add_child_result.value().error_value()));
    CompleteStart(zx::error(ZX_ERR_INTERNAL));
    return;
  }

  FDF_LOG(INFO, "Successfully started aml-uart-dfv2 driver.");
  CompleteStart(zx::ok());
}

void AmlUartV2::CompleteStart(zx::result<> result) {
  ZX_ASSERT(start_completer_.has_value());
  start_completer_.value()(result);
  start_completer_.reset();
}

}  // namespace serial

FUCHSIA_DRIVER_EXPORT(serial::AmlUartV2);
