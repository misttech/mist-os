// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_SSHD_HOST_SERVICE_H_
#define SRC_DEVELOPER_SSHD_HOST_SERVICE_H_

#include <fuchsia/component/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/sys/cpp/service_directory.h>
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
    const std::shared_ptr<sys::ServiceDirectory>& service_directory);

// Service relies on the default async dispatcher and is not thread safe.
class Service {
 public:
  Service(async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> service_directory,
          uint16_t port);
  ~Service();

 private:
  enum class IpVersion { V4 = AF_INET, V6 = AF_INET6 };

  struct Socket {
    fbl::unique_fd fd;
    fsl::FDWaiter waiter;
  };

  static Socket MakeSocket(async_dispatcher_t* dispatcher, IpVersion ip_version, uint16_t port);

  void Wait(std::optional<IpVersion> ip_version);
  void Launch(fbl::unique_fd conn);

  async_dispatcher_t* dispatcher_;
  std::shared_ptr<sys::ServiceDirectory> service_directory_;
  uint16_t port_;
  // TODO(https://fxbug.dev/21198): Replace these with a single dual-stack
  // socket once Netstack3 supports that.
  Socket v4_socket_, v6_socket_;
  uint64_t next_child_num_ = 0;

  std::vector<fuchsia::component::ExecutionControllerPtr> controllers_;
};

}  // namespace sshd_host

#endif  // SRC_DEVELOPER_SSHD_HOST_SERVICE_H_
