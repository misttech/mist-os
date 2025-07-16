// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/virtualization/lib/guest_interaction/interactive_debian_guest/running_guest.h"

#include <lib/syslog/cpp/macros.h>

#include "src/virtualization/lib/grpc/fdio_util.h"

namespace interactive_debian_guest {

RunningGuest::RunningGuest(std::string name, fbl::unique_fd vsock_fd,
                           async_dispatcher_t* dispatcher,
                           fidl::SyncClient<fuchsia_virtualization::Guest> guest_sync)
    : name_(std::move(name)), dispatcher_(dispatcher), guest_sync_(std::move(guest_sync)) {
  // SetNonBlocking defined in src/virtualization/lib/grpc/fdio_util.h
  int ret = SetNonBlocking(vsock_fd);
  FX_CHECK(ret == 0) << std::format(
      "[{}] Failed to set the vsock port as non-blocking with errno: {}", Name(), strerror(ret));

  interaction_client_ = std::make_unique<ClientImpl<PosixPlatform>>(vsock_fd.release());

  int thrd_ret = interaction_client_->Start(guest_interaction_service_thread_);
  FX_CHECK(thrd_ret == thrd_success)
      << std::format("[{}] Failed to start guest interaction client thread with errno: {}", Name(),
                     strerror(thrd_ret));
}

RunningGuest::~RunningGuest() {
  // All teardowns should be gracefully done by the owning class, and failure to do so
  // puts the interaction client in an indeterminte state with regards to bound ports.
  FX_CHECK(interaction_client_ && interaction_client_->IsRunning())
      << "An invalid teardown sequence has occurred.";
}

void RunningGuest::PutFile(fidl::ClientEnd<fuchsia_io::File> host_source,
                           const std::string& guest_dest, FileTransferCallback result) {
  FX_CHECK(interaction_client_ && interaction_client_->IsRunning());
  interaction_client_->Put(std::move(host_source), guest_dest, std::move(result));
}

void RunningGuest::GetFile(const std::string& guest_source,
                           fidl::ClientEnd<fuchsia_io::File> host_dest,
                           FileTransferCallback result) {
  FX_CHECK(interaction_client_ && interaction_client_->IsRunning());
  interaction_client_->Get(guest_source, std::move(host_dest), std::move(result));
}

void RunningGuest::Execute(
    std::string command, std::map<std::string, std::string> env_variables, zx::socket std_in,
    zx::socket std_out, zx::socket std_err,
    fidl::ServerEnd<fuchsia_virtualization_guest_interaction::CommandListener> listener) {
  interaction_client_->Exec(std::move(command), env_variables, std::move(std_in),
                            std::move(std_out), std::move(std_err), std::move(listener),
                            dispatcher_);
}

void RunningGuest::Shutdown() {
  FX_LOGST(INFO, Name()) << "Beginning shutdown procedures.";
  interaction_client_->Stop();
}

const char* RunningGuest::Name() { return name_.c_str(); }

}  // namespace interactive_debian_guest
