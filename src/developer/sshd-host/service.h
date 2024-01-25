// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_SSHD_HOST_SERVICE_H_
#define SRC_DEVELOPER_SSHD_HOST_SERVICE_H_

#include <fidl/fuchsia.boot/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/process.h>
#include <sys/socket.h>

#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include <fbl/unique_fd.h>

#include "src/lib/fsl/tasks/fd_waiter.h"

namespace sshd_host {

inline constexpr char kSshDirectory[] = "/data/ssh";
inline constexpr char kAuthorizedKeysPath[] = "/data/ssh/authorized_keys";
// Name of the collection that contains sshd shell child components.
inline constexpr std::string_view kShellCollection = "shell";

zx_status_t provision_authorized_keys_from_bootloader_file(
    fidl::SyncClient<fuchsia_boot::Items>& boot_items);

class Service;

// Service relies on the default async dispatcher and is not thread safe.
class Service {
 public:
  Service(async_dispatcher_t* dispatcher, uint16_t port);
  ~Service();

 private:
  enum class IpVersion { V4 = AF_INET, V6 = AF_INET6 };
  struct Controller;

  struct Socket {
    fbl::unique_fd fd;
    fsl::FDWaiter waiter;
  };

  static Socket MakeSocket(async_dispatcher_t* dispatcher, IpVersion ip_version, uint16_t port);

  void Wait(std::optional<IpVersion> ip_version);
  void Launch(fbl::unique_fd conn);

  void OnStop(zx_status_t status, Controller* controller);

  async_dispatcher_t* dispatcher_;
  uint16_t port_;
  // TODO(https://fxbug.dev/42095034): Replace these with a single dual-stack
  // socket once Netstack3 supports that.
  Socket v4_socket_, v6_socket_;
  uint64_t next_child_num_ = 0;

  struct Controller final : public fidl::AsyncEventHandler<fuchsia_component::ExecutionController> {
    Controller(Service* service, uint64_t child_num, std::string child_name,
               fidl::ClientEnd<fuchsia_component::ExecutionController> client_end,
               async_dispatcher_t* dispatcher, fidl::SyncClient<fuchsia_component::Realm> realm)
        : service_(service),
          child_num_(child_num),
          child_name_(std::move(child_name)),
          client_(std::move(client_end), dispatcher, this),
          realm_(std::move(realm)) {}
    void OnStop(fidl::Event<fuchsia_component::ExecutionController::OnStop>& event) override {
      service_->OnStop(event.stopped_payload().status().value_or(ZX_OK), this);
    }
    void on_fidl_error(fidl::UnbindInfo error) override {
      service_->OnStop(error.ToError().status(), this);
    }

    fidl::Client<fuchsia_component::ExecutionController>& operator->() { return client_; }

    Service* service_;
    uint64_t child_num_;
    std::string child_name_;
    fidl::Client<fuchsia_component::ExecutionController> client_;
    fidl::SyncClient<fuchsia_component::Realm> realm_;
  };

  std::map<uint64_t, Controller> controllers_;
};

}  // namespace sshd_host

#endif  // SRC_DEVELOPER_SSHD_HOST_SERVICE_H_
