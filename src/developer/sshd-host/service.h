// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_SSHD_HOST_SERVICE_H_
#define SRC_DEVELOPER_SSHD_HOST_SERVICE_H_

#include <lib/sys/cpp/service_directory.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <sys/socket.h>

#include <optional>
#include <string>
#include <vector>

#include "constants.h"
#include "fbl/unique_fd.h"
#include "src/lib/fsl/tasks/fd_waiter.h"

namespace sshd_host {

constexpr zx_rights_t kChildJobRights =
    ZX_RIGHTS_BASIC | ZX_RIGHT_DESTROY | ZX_RIGHT_MANAGE_PROCESS | ZX_RIGHT_MANAGE_JOB;

constexpr char kSshDirectory[] = "/data/ssh";
constexpr char kAuthorizedKeysPath[] = "/data/ssh/authorized_keys";

zx_status_t provision_authorized_keys_from_bootloader_file(
    std::shared_ptr<sys::ServiceDirectory> service_directory);

zx_status_t make_child_job(const zx::job& parent, std::string name, zx::job* job);

// Service relies on the default async dispatcher and is not thread safe.
class Service {
 public:
  explicit Service(uint16_t port);
  ~Service();

 private:
  enum class IpVersion { V4 = AF_INET, V6 = AF_INET6 };

  struct Socket {
    fbl::unique_fd fd;
    fsl::FDWaiter waiter;
  };

  static fbl::unique_fd MakeSocket(IpVersion ip_version, uint16_t port);

  void Wait(std::optional<IpVersion> ip_version);
  void Launch(int conn, const std::string& peer_name);
  void ProcessTerminated(zx::process process, zx::job job);

  uint16_t port_;
  // TODO(https://fxbug.dev/21198): Replace these with a single dual-stack
  // socket once Netstack3 supports that.
  Socket v4_socket_, v6_socket_;
  zx::job job_;

  std::vector<std::unique_ptr<async::Wait>> process_waiters_;
};

}  // namespace sshd_host

#endif  // SRC_DEVELOPER_SSHD_HOST_SERVICE_H_
