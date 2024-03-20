// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#if __Fuchsia_API_level__ >= 15

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/inspect/component/cpp/component.h>

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
    zx::result logger = Logger::Create(incoming, dispatcher_, name_, FUCHSIA_LOG_INFO,
                                       logger_wait_for_initial_interest);
    ZX_ASSERT_MSG(logger.is_ok(), "%s", logger.status_string());
    return std::move(logger.value());
  }();
  Logger::SetGlobalInstance(logger_.get());
  std::optional outgoing_request = std::move(start_args_.outgoing_dir());
  ZX_ASSERT(outgoing_request.has_value());
  InitializeAndServe(std::move(incoming), std::move(outgoing_request.value()));

#if __Fuchsia_API_level__ >= 19
  const auto& node_properties = start_args_.node_properties();
  if (node_properties.has_value()) {
    for (const auto& entry : node_properties.value()) {
      node_properties_.emplace(std::string{entry.name()}, entry.properties());
    }
  }
#endif
}

void DriverBase::InitializeAndServe(
    Namespace incoming, fidl::ServerEnd<fuchsia_io::Directory> outgoing_directory_request) {
  incoming_ = std::make_shared<Namespace>(std::move(incoming));
  outgoing_ =
      std::make_shared<OutgoingDirectory>(OutgoingDirectory::Create(driver_dispatcher_->get()));
  ZX_ASSERT(outgoing_->Serve(std::move(outgoing_directory_request)).is_ok());
}

void DriverBase::InitInspectorExactlyOnce(inspect::Inspector inspector) {
  std::call_once(init_inspector_once_, [&] {
#if __Fuchsia_API_level__ >= 16
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

cpp20::span<const fuchsia_driver_framework::NodeProperty> DriverBase::node_properties(
    const std::string& parent_node_name) const {
  auto it = node_properties_.find(parent_node_name);
  if (it == node_properties_.end()) {
    return {};
  }
  return {it->second};
}

DriverBase::~DriverBase() { Logger::SetGlobalInstance(nullptr); }

}  // namespace fdf

#endif
