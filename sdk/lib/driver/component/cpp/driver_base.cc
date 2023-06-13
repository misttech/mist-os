// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_base.h>

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
  std::optional outgoing_request = std::move(start_args_.outgoing_dir());
  ZX_ASSERT(outgoing_request.has_value());
  InitializeAndServe(std::move(incoming), std::move(outgoing_request.value()));
}

void DriverBase::InitializeAndServe(
    Namespace incoming, fidl::ServerEnd<fuchsia_io::Directory> outgoing_directory_request) {
  incoming_ = std::make_shared<Namespace>(std::move(incoming));
  outgoing_ =
      std::make_shared<OutgoingDirectory>(OutgoingDirectory::Create(driver_dispatcher_->get()));
  ZX_ASSERT(outgoing_->Serve(std::move(outgoing_directory_request)).is_ok());
}

}  // namespace fdf
