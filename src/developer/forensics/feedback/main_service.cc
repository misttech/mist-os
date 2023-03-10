// Copyright 2021 The Fuchsia Authors.All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/main_service.h"

#include <fuchsia/feedback/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <vector>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/feedback/annotations/device_id_provider.h"
#include "src/developer/forensics/feedback/annotations/provider.h"
#include "src/developer/forensics/feedback/redactor_factory.h"
#include "src/developer/forensics/utils/cobalt/logger.h"
#include "src/lib/backoff/exponential_backoff.h"
#include "src/lib/timekeeper/system_clock.h"

namespace forensics::feedback {
namespace {

std::unique_ptr<CachedAsyncAnnotationProvider> MakeDeviceIdProvider(
    const std::optional<std::string>& local_device_id_path, async_dispatcher_t* dispatcher,
    const std::shared_ptr<sys::ServiceDirectory>& services) {
  if (local_device_id_path.has_value()) {
    FX_LOGS(INFO) << "Using local device id provider";
    return std::make_unique<LocalDeviceIdProvider>(local_device_id_path.value());
  }

  FX_LOGS(INFO) << "Using remote device id provider";
  return std::make_unique<RemoteDeviceIdProvider>(dispatcher, services,
                                                  AnnotationProviders::AnnotationProviderBackoff());
}

}  // namespace

MainService::MainService(async_dispatcher_t* dispatcher,
                         std::shared_ptr<sys::ServiceDirectory> services, timekeeper::Clock* clock,
                         inspect::Node* inspect_root, cobalt::Logger* cobalt,
                         const Annotations& startup_annotations, Options options)
    : dispatcher_(dispatcher),
      services_(services),
      clock_(clock),
      inspect_root_(inspect_root),
      cobalt_(cobalt),
      redactor_(RedactorFromConfig(inspect_root, options.build_type_config)),
      inspect_node_manager_(inspect_root),
      annotations_(dispatcher_, services_,
                   options.feedback_data_options.config.annotation_allowlist, startup_annotations,
                   MakeDeviceIdProvider(options.local_device_id_path, dispatcher_, services_)),
      feedback_data_(dispatcher_, services_, clock_, inspect_root_, cobalt_, redactor_.get(),
                     annotations_.GetAnnotationManager(), options.feedback_data_options),
      crash_reports_(dispatcher_, services_, clock_, inspect_root_,
                     annotations_.GetAnnotationManager(), feedback_data_.DataProvider(),
                     options.crash_reports_options),
      last_reboot_(dispatcher_, services_, cobalt_, redactor_.get(), crash_reports_.CrashReporter(),
                   options.last_reboot_options),
      component_data_register_bindings_(dispatcher_, annotations_.ComponentDataRegister(),
                                        &inspect_node_manager_,
                                        "/fidl/fuchsia.feedback.ComponentDataRegister"),
      crash_reporter_bindings_(dispatcher_, crash_reports_.CrashReporter(), &inspect_node_manager_,
                               "/fidl/fuchsia.feedback.CrashReporter"),
      crash_register_bindings_(dispatcher_, crash_reports_.CrashRegister(), &inspect_node_manager_,
                               "/fidl/fuchsia.feedback.CrashReportingProductRegister"),
      data_provider_bindings_(dispatcher_, feedback_data_.DataProvider(), &inspect_node_manager_,
                              "/fidl/fuchsia.feedback.DataProvider"),
      data_provider_controller_bindings_(dispatcher_, feedback_data_.DataProviderController(),
                                         &inspect_node_manager_,
                                         "/fidl/fuchsia.feedback.DataProviderController"),
      last_reboot_info_bindings_(dispatcher_, last_reboot_.LastRebootInfoProvider(),
                                 &inspect_node_manager_,
                                 "/fidl/fuchsia.feedback.LastRebootInfoProvider") {}

void MainService::ShutdownImminent(::fit::deferred_callback stop_respond) {
  crash_reports_.ShutdownImminent();
  feedback_data_.ShutdownImminent(std::move(stop_respond));
}

template <>
::fidl::InterfaceRequestHandler<fuchsia::feedback::LastRebootInfoProvider>
MainService::GetHandler() {
  return [this](::fidl::InterfaceRequest<fuchsia::feedback::LastRebootInfoProvider> request) {
    last_reboot_info_bindings_.AddBinding(std::move(request));
  };
}

template <>
::fidl::InterfaceRequestHandler<fuchsia::feedback::CrashReporter> MainService::GetHandler() {
  return [this](::fidl::InterfaceRequest<fuchsia::feedback::CrashReporter> request) {
    crash_reporter_bindings_.AddBinding(std::move(request));
  };
}

template <>
::fidl::InterfaceRequestHandler<fuchsia::feedback::CrashReportingProductRegister>
MainService::GetHandler() {
  return
      [this](::fidl::InterfaceRequest<fuchsia::feedback::CrashReportingProductRegister> request) {
        crash_register_bindings_.AddBinding(std::move(request));
      };
}

template <>
::fidl::InterfaceRequestHandler<fuchsia::feedback::ComponentDataRegister>
MainService::GetHandler() {
  return [this](::fidl::InterfaceRequest<fuchsia::feedback::ComponentDataRegister> request) {
    component_data_register_bindings_.AddBinding(std::move(request));
  };
}

template <>
::fidl::InterfaceRequestHandler<fuchsia::feedback::DataProvider> MainService::GetHandler() {
  return [this](::fidl::InterfaceRequest<fuchsia::feedback::DataProvider> request) {
    data_provider_bindings_.AddBinding(std::move(request));
  };
}

template <>
::fidl::InterfaceRequestHandler<fuchsia::feedback::DataProviderController>
MainService::GetHandler() {
  return [this](::fidl::InterfaceRequest<fuchsia::feedback::DataProviderController> request) {
    data_provider_controller_bindings_.AddBinding(std::move(request));
  };
}

}  // namespace forensics::feedback
