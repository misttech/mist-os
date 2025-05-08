// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/cpu/drivers/aml-cpu/aml-cpu-driver.h"

#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>

#include <soc/aml-common/aml-cpu-metadata.h>

namespace amlogic_cpu {

AmlCpuDriver::AmlCpuDriver(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("aml-cpu", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> AmlCpuDriver::Start() {
  // Get the metadata for the performance domains.
  auto perf_doms =
      compat::GetMetadataArray<perf_domain_t>(incoming(), DEVICE_METADATA_AML_PERF_DOMAINS, "pdev");
  if (perf_doms.is_error()) {
    FDF_LOG(ERROR, "Failed to get performance domains from board driver, st = %s",
            perf_doms.status_string());
    return zx::error(perf_doms.take_error());
  }

  auto pdev_conn = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
  if (pdev_conn.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to platform device, error = %s", pdev_conn.status_string());
    return zx::error(pdev_conn.take_error());
  }
  fdf::PDev pdev{std::move(pdev_conn.value())};

  auto config = LoadConfiguration(pdev);
  if (config.is_error()) {
    FDF_LOG(ERROR, "Failed to load cpu configuration: %s", config.status_string());
    return zx::error(config.take_error());
  }

  auto op_points =
      compat::GetMetadataArray<operating_point_t>(incoming(), config->metadata_type, "pdev");
  if (op_points.is_error()) {
    FDF_LOG(ERROR, "Failed to get operating point from board driver: %s",
            op_points.status_string());
    return zx::error(op_points.error_value());
  }

  node_.Bind(std::move(node()));

  // Build and publish each performance domain.
  for (const perf_domain_t& perf_domain : perf_doms.value()) {
    // Vector of operating points that belong to this power domain.
    std::vector<operating_point_t> pd_op_points =
        PerformanceDomainOpPoints(perf_domain, op_points.value());
    auto device = BuildPerformanceDomain(perf_domain, pd_op_points, config.value());
    if (device.is_error()) {
      FDF_LOG(ERROR, "Failed to build performance domain node: %s", device.status_string());
      return zx::error(device.error_value());
    }

    auto st = device->AddChild(node_);
    if (st.is_error()) {
      FDF_LOG(ERROR, "Failed to add child for performance domain: %s", st.status_string());
      return st.take_error();
    }
    fuchsia_hardware_cpu_ctrl::Service::InstanceHandler handler({
        .device = fit::bind_member<&AmlCpuPerformanceDomain::CpuCtrlConnector>(&**device),
    });

    auto result = outgoing()->AddService<fuchsia_hardware_cpu_ctrl::Service>(std::move(handler),
                                                                             device->GetName());
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add service: %s", result.status_string());
      return result.take_error();
    }

    performance_domains_.push_back(std::move(device.value()));
  }

  return zx::ok();
}

zx::result<std::unique_ptr<AmlCpuPerformanceDomain>> AmlCpuDriver::BuildPerformanceDomain(
    const perf_domain_t& perf_domain, const std::vector<operating_point>& pd_op_points,
    const AmlCpuConfiguration& config) {
  char fragment_name[32];
  fidl::ClientEnd<fuchsia_hardware_clock::Clock> pll_div16_client;
  fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpu_div16_client;
  if (config.has_div16_clients) {
    snprintf(fragment_name, sizeof(fragment_name), "clock-pll-div16-%02d", perf_domain.id);
    zx::result pll_clock_client =
        incoming()->Connect<fuchsia_hardware_clock::Service::Clock>(fragment_name);
    if (pll_clock_client.is_error()) {
      FDF_LOG(ERROR, "Failed to get clock protocol from fragment '%s': %s\n", fragment_name,
              pll_clock_client.status_string());
      return zx::error(pll_clock_client.status_value());
    }
    pll_div16_client = std::move(*pll_clock_client);

    snprintf(fragment_name, sizeof(fragment_name), "clock-cpu-div16-%02d", perf_domain.id);
    zx::result cpu_clock_client =
        incoming()->Connect<fuchsia_hardware_clock::Service::Clock>(fragment_name);
    if (cpu_clock_client.is_error()) {
      FDF_LOG(ERROR, "Failed to get clock protocol from fragment '%s': %s\n", fragment_name,
              cpu_clock_client.status_string());
      return zx::error(cpu_clock_client.status_value());
    }
    cpu_div16_client = std::move(*cpu_clock_client);
  }

  snprintf(fragment_name, sizeof(fragment_name), "clock-cpu-scaler-%02d", perf_domain.id);
  zx::result clock_client =
      incoming()->Connect<fuchsia_hardware_clock::Service::Clock>(fragment_name);
  if (clock_client.is_error()) {
    FDF_LOG(ERROR, "Failed to get clock protocol from fragment '%s': %s\n", fragment_name,
            clock_client.status_string());
    return zx::error(clock_client.status_value());
  }
  fidl::ClientEnd<fuchsia_hardware_clock::Clock> cpu_scaler_client{std::move(*clock_client)};

  // For A1, the CPU power is VDD_CORE, which share with other module.
  // The fixed voltage is 0.8v, we can't adjust it dynamically.
  fidl::ClientEnd<fuchsia_hardware_power::Device> power_client;
  if (config.has_power_client) {
    snprintf(fragment_name, sizeof(fragment_name), "power-%02d", perf_domain.id);
    zx::result client_end_result =
        incoming()->Connect<fuchsia_hardware_power::Service::Device>(fragment_name);
    if (client_end_result.is_error()) {
      FDF_LOG(ERROR, "Failed to create power client, st = %s", client_end_result.status_string());
      return zx::error(client_end_result.error_value());
    }

    power_client = std::move(client_end_result.value());
  }

  auto device = std::make_unique<AmlCpuPerformanceDomain>(dispatcher(), pd_op_points, perf_domain,
                                                          inspector());

  auto st = device->Init(std::move(pll_div16_client), std::move(cpu_div16_client),
                         std::move(cpu_scaler_client), std::move(power_client));
  if (st != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize device: %s", zx_status_get_string(st));
    return zx::error(st);
  }

  device->SetCpuInfo(config.cpu_version_packed);

  return zx::ok(std::move(device));
}

void AmlCpuPerformanceDomain::CpuCtrlConnector(
    fidl::ServerEnd<fuchsia_hardware_cpu_ctrl::Device> server) {
  FDF_LOG(INFO, "Binding domain to server");
  bindings_.AddBinding(dispatcher_, std::move(server), this, fidl::kIgnoreBindingClosure);
}

zx::result<> AmlCpuPerformanceDomain::AddChild(
    fidl::WireSyncClient<fuchsia_driver_framework::Node>& node) {
  fidl::Arena arena;

  zx::result connector = devfs_connector_.Bind(dispatcher_);
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .connector_supports(fuchsia_device_fs::ConnectionType::kDevice |
                                       fuchsia_device_fs::ConnectionType::kController)
                   .class_name("cpu-ctrl");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, GetName())
                  .devfs_args(devfs.Build())
                  .Build();

  // Create endpoints of the `NodeController` for the node.
  auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoint: %s", endpoints.status_string());
    return zx::error(endpoints.status_value());
  }

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (node_endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create node endpoint: %s", node_endpoints.status_string());
    return zx::error(node_endpoints.status_value());
  }

  auto result =
      node->AddChild(args, std::move(endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(endpoints->client));
  node_.Bind(std::move(node_endpoints->client));

  return zx::ok();
}

}  // namespace amlogic_cpu

FUCHSIA_DRIVER_EXPORT(amlogic_cpu::AmlCpuDriver);
