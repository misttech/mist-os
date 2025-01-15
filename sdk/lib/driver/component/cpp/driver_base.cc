// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/inspect/component/cpp/component.h>
#include <zircon/availability.h>

#include <mutex>

namespace fdf {

__WEAK bool logger_wait_for_initial_interest = true;

DriverBase::DriverBase(std::string_view name, DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : name_(name),
      start_args_(std::move(start_args)),
      driver_dispatcher_(std::move(driver_dispatcher)),
      dispatcher_(driver_dispatcher_->async_dispatcher()) {
  Namespace incoming = [ns = std::move(start_args_.incoming())]() mutable {
    ZX_ASSERT(ns.has_value());
    zx::result incoming = Namespace::Create(ns.value());
    ZX_ASSERT_MSG(incoming.is_ok(), "%s", incoming.status_string());
    return std::move(incoming.value());
  }();
  logger_ = [&incoming, this]() {
#if FUCHSIA_API_LEVEL_AT_LEAST(24)
    auto logger = Logger::Create2(incoming, dispatcher_, name_, FUCHSIA_LOG_INFO,
                                  logger_wait_for_initial_interest);
    return logger;
#else
    zx::result logger = Logger::Create(incoming, dispatcher_, name_, FUCHSIA_LOG_INFO,
                                       logger_wait_for_initial_interest);
    ZX_ASSERT_MSG(logger.is_ok(), "%s", logger.status_string());
    return std::move(logger.value());
#endif
  }();
  Logger::SetGlobalInstance(logger_.get());
  std::optional outgoing_request = std::move(start_args_.outgoing_dir());
  ZX_ASSERT(outgoing_request.has_value());
  InitializeAndServe(std::move(incoming), std::move(outgoing_request.value()));

#if FUCHSIA_API_LEVEL_AT_LEAST(19) && FUCHSIA_API_LEVEL_AT_MOST(26)
  const auto& node_properties = start_args_.node_properties();
  if (node_properties.has_value()) {
    for (const auto& entry : node_properties.value()) {
      node_properties_.emplace(std::string{entry.name()}, entry.properties());
    }
  }
#endif

#if FUCHSIA_API_LEVEL_AT_LEAST(26)
  const auto& node_properties_2 = start_args_.node_properties_2();
  if (node_properties_2.has_value()) {
    for (const auto& entry : node_properties_2.value()) {
      node_properties_2_.emplace(std::string{entry.name()}, entry.properties());
    }
  }
#endif

#if FUCHSIA_API_LEVEL_AT_LEAST(25)
  zx::result val = fdf_internal::ProgramValue(program(), "service_connect_validation");
  if (val.is_ok() && val.value() == "true") {
    EnableServiceValidator();
  }
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(25)

#if FUCHSIA_API_LEVEL_AT_LEAST(26)
#endif
}

void DriverBase::InitializeAndServe(
    Namespace incoming, fidl::ServerEnd<fuchsia_io::Directory> outgoing_directory_request) {
  incoming_ = std::make_shared<Namespace>(std::move(incoming));
  outgoing_ =
      std::make_shared<OutgoingDirectory>(OutgoingDirectory::Create(driver_dispatcher_->get()));
  ZX_ASSERT(outgoing_->Serve(std::move(outgoing_directory_request)).is_ok());
}

#if FUCHSIA_API_LEVEL_AT_LEAST(25)
void DriverBase::EnableServiceValidator() {
  if (start_args_.node_offers().has_value()) {
    incoming_->SetServiceValidator(
        std::make_optional<ServiceValidator>(start_args_.node_offers().value()));
  } else {
    FDF_LOGL(INFO, *logger_, "No node_offers available, not able to enable service validation.");
  }
}
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(25)

void DriverBase::InitInspectorExactlyOnce(inspect::Inspector inspector) {
  std::call_once(init_inspector_once_, [&] {
#if FUCHSIA_API_LEVEL_AT_LEAST(16)
    inspector_.emplace(
        dispatcher(), inspect::PublishOptions{
                          .inspector = std::move(inspector),
                          .tree_name = {name_},
                          .client_end = incoming()->Connect<fuchsia_inspect::InspectSink>().value(),
                      });
#else
    inspector_.emplace(outgoing()->component(), dispatcher(), std::move(inspector));
#endif
  });
}

#if FUCHSIA_API_LEVEL_AT_MOST(26)
cpp20::span<const fuchsia_driver_framework::NodeProperty> DriverBase::node_properties(
    const std::string& parent_node_name) const {
  auto it = node_properties_.find(parent_node_name);
  if (it == node_properties_.end()) {
    return {};
  }
  return {it->second};
}
#endif

#if FUCHSIA_API_LEVEL_AT_LEAST(18)

zx::result<OwnedChildNode> DriverBase::AddOwnedChild(std::string_view node_name) {
  return fdf::AddOwnedChild(node(), logger(), node_name);
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> DriverBase::AddChild(
    std::string_view node_name,
    cpp20::span<const fuchsia_driver_framework::NodeProperty> properties,
    cpp20::span<const fuchsia_driver_framework::Offer> offers) {
  return fdf::AddChild(node(), logger(), node_name, properties, offers);
}

zx::result<OwnedChildNode> DriverBase::AddOwnedChild(
    std::string_view node_name, fuchsia_driver_framework::DevfsAddArgs& devfs_args) {
  return fdf::AddOwnedChild(node(), logger(), node_name, devfs_args);
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> DriverBase::AddChild(
    std::string_view node_name, fuchsia_driver_framework::DevfsAddArgs& devfs_args,
    cpp20::span<const fuchsia_driver_framework::NodeProperty> properties,
    cpp20::span<const fuchsia_driver_framework::Offer> offers) {
  return fdf::AddChild(node(), logger(), node_name, devfs_args, properties, offers);
}

#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)

#if FUCHSIA_API_LEVEL_AT_LEAST(26)

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> DriverBase::AddChild(
    std::string_view node_name,
    cpp20::span<const fuchsia_driver_framework::NodeProperty2> properties,
    cpp20::span<const fuchsia_driver_framework::Offer> offers) {
  return fdf::AddChild(node(), logger(), node_name, properties, offers);
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> DriverBase::AddChild(
    std::string_view node_name, fuchsia_driver_framework::DevfsAddArgs& devfs_args,
    cpp20::span<const fuchsia_driver_framework::NodeProperty2> properties,
    cpp20::span<const fuchsia_driver_framework::Offer> offers) {
  return fdf::AddChild(node(), logger(), node_name, devfs_args, properties, offers);
}

cpp20::span<const fuchsia_driver_framework::NodeProperty2> DriverBase::node_properties_2(
    const std::string& parent_node_name) const {
  auto it = node_properties_2_.find(parent_node_name);
  if (it == node_properties_2_.end()) {
    return {};
  }
  return {it->second};
}

#endif

DriverBase::~DriverBase() { Logger::SetGlobalInstance(nullptr); }

}  // namespace fdf
