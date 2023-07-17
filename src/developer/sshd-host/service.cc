// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/sshd-host/service.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <fuchsia/boot/cpp/fidl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/process/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fdio/fd.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/syslog/cpp/macros.h>
#include <netdb.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>
#include <zircon/types.h>

#include <memory>
#include <vector>

#include <fbl/unique_fd.h>

#include "src/developer/sshd-host/constants.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace sshd_host {

zx_status_t provision_authorized_keys_from_bootloader_file(
    const std::shared_ptr<sys::ServiceDirectory>& service_directory) {
  fuchsia::boot::ItemsSyncPtr boot_items;
  if (zx_status_t status = service_directory->Connect(boot_items.NewRequest()); status != ZX_OK) {
    FX_PLOGS(ERROR, status)
        << "Provisioning keys from boot item: failed to connect to boot items service";
    return status;
  }

  zx::vmo vmo;
  if (zx_status_t status =
          boot_items->GetBootloaderFile(std::string(kAuthorizedKeysBootloaderFileName.data(),
                                                    kAuthorizedKeysBootloaderFileName.size()),
                                        &vmo);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Provisioning keys from boot item: GetBootloaderFile failed";
    return status;
  }

  if (!vmo.is_valid()) {
    FX_LOGS(INFO) << "Provisioning keys from boot item: bootloader file not found: "
                  << kAuthorizedKeysBootloaderFileName;
    return ZX_ERR_NOT_FOUND;
  }

  uint64_t size;
  if (zx_status_t status = vmo.get_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Provisioning keys from boot item: unable to get file size";
    return status;
  }

  std::unique_ptr buffer = std::make_unique<uint8_t[]>(size);
  if (zx_status_t status = vmo.read(buffer.get(), 0, size); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Provisioning keys from boot item: failed to read file";
    return status;
  }

  if (mkdir(kSshDirectory, 0700) && errno != EEXIST) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: failed to create directory: "
                   << kSshDirectory << " Error: " << strerror(errno);
    return ZX_ERR_IO;
  }

  fbl::unique_fd kfd(open(kAuthorizedKeysPath, O_CREAT | O_EXCL | O_WRONLY, S_IRUSR | S_IWUSR));
  if (!kfd) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: open failed: " << kAuthorizedKeysPath
                   << " error: " << strerror(errno);
    return errno == EEXIST ? ZX_ERR_ALREADY_EXISTS : ZX_ERR_IO;
  }

  if (write(kfd.get(), buffer.get(), size) != static_cast<ssize_t>(size)) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: write failed: " << strerror(errno);
    return ZX_ERR_IO;
  }

  fsync(kfd.get());

  if (close(kfd.release())) {
    FX_LOGS(ERROR) << "Provisioning keys from boot item: close failed: " << strerror(errno);
    return ZX_ERR_IO;
  }

  FX_LOGS(INFO) << "Provisioning keys from boot item: authorized_keys provisioned";
  return ZX_OK;
}

// static
Service::Socket Service::MakeSocket(async_dispatcher_t* dispatcher, IpVersion ip_version,
                                    uint16_t port) {
  const int family = static_cast<int>(ip_version);
  fbl::unique_fd sock(socket(family, SOCK_STREAM, IPPROTO_TCP));
  if (!sock.is_valid()) {
    FX_LOGS(FATAL) << "Failed to create socket: " << strerror(errno);
  }
  sockaddr_storage addr;
  switch (ip_version) {
    case IpVersion::V4:
      *reinterpret_cast<struct sockaddr_in*>(&addr) = sockaddr_in{
          .sin_family = AF_INET,
          .sin_port = htons(port),
          .sin_addr = in_addr{INADDR_ANY},
      };
      break;
    case IpVersion::V6:
      *reinterpret_cast<struct sockaddr_in6*>(&addr) = sockaddr_in6{
          .sin6_family = AF_INET6,
          .sin6_port = htons(port),
          .sin6_addr = in6addr_any,
      };
      // Disable dual-stack mode for the socket.
      constexpr const int kEnable = 1;
      setsockopt(sock.get(), IPPROTO_IPV6, IPV6_V6ONLY, &kEnable, sizeof(kEnable));
      break;
  }
  if (bind(sock.get(), reinterpret_cast<const sockaddr*>(&addr), sizeof addr) < 0) {
    FX_LOGS(FATAL) << "Failed to bind to " << port << ": " << strerror(errno);
  }

  FX_SLOG(INFO, "listen() for inbound SSH connections", KV("port", (int)port));
  if (listen(sock.get(), 10) < 0) {
    FX_LOGS(FATAL) << "Failed to listen: " << strerror(errno);
  }

  return Socket{.fd = std::move(sock), .waiter = fsl::FDWaiter(dispatcher)};
}

Service::Service(async_dispatcher_t* dispatcher,
                 std::shared_ptr<sys::ServiceDirectory> service_directory, uint16_t port)
    : dispatcher_(dispatcher),
      service_directory_(std::move(service_directory)),
      port_(port),
      v4_socket_(MakeSocket(dispatcher_, IpVersion::V4, port_)),
      v6_socket_(MakeSocket(dispatcher_, IpVersion::V6, port_)) {
  Wait(std::nullopt);
}

Service::~Service() = default;

void Service::Wait(std::optional<IpVersion> ip_version) {
  FX_SLOG(INFO, "Waiting for next connection");

  auto do_wait = [this](Service::Socket* sock, IpVersion ip_version) {
    sock->waiter.Wait(
        [this, sock, ip_version](zx_status_t status, uint32_t /*events*/) {
          if (status != ZX_OK) {
            FX_PLOGS(FATAL, status) << "Failed to wait on socket";
          }

          struct sockaddr_storage peer_addr {};
          socklen_t peer_addr_len = sizeof(peer_addr);
          fbl::unique_fd conn(accept(sock->fd.get(), reinterpret_cast<struct sockaddr*>(&peer_addr),
                                     &peer_addr_len));
          if (!conn.is_valid()) {
            if (errno == EPIPE) {
              FX_LOGS(FATAL) << "The netstack died. Terminating.";
            } else {
              FX_LOGS(ERROR) << "Failed to accept: " << strerror(errno);
              // Wait for another connection.
              Wait(ip_version);
            }
            return;
          }

          std::string peer_name = "unknown";
          char host[NI_MAXHOST];
          char port[NI_MAXSERV];
          if (int res =
                  getnameinfo(reinterpret_cast<struct sockaddr*>(&peer_addr), peer_addr_len, host,
                              sizeof(host), port, sizeof(port), NI_NUMERICHOST | NI_NUMERICSERV);
              res == 0) {
            switch (ip_version) {
              case IpVersion::V4:
                peer_name = fxl::StringPrintf("%s:%s", host, port);
                break;
              case IpVersion::V6:
                peer_name = fxl::StringPrintf("[%s]:%s", host, port);
                break;
            }
          } else {
            FX_LOGS(WARNING)
                << "Error from getnameinfo(.., NI_NUMERICHOST | NI_NUMERICSERV) for peer address: "
                << gai_strerror(res);
          }
          FX_SLOG(INFO, "Accepted connection", KV("remote", peer_name.c_str()));

          Launch(std::move(conn));
          Wait(ip_version);
        },
        sock->fd.get(), POLLIN);
  };

  if (ip_version) {
    switch (*ip_version) {
      case IpVersion::V4:
        return do_wait(&v4_socket_, IpVersion::V4);
      case IpVersion::V6:
        return do_wait(&v6_socket_, IpVersion::V6);
    }
  } else {
    do_wait(&v4_socket_, IpVersion::V4);
    do_wait(&v6_socket_, IpVersion::V6);
  }
}

void Service::Launch(fbl::unique_fd conn) {
  std::string child_name = fxl::StringPrintf("sshd-%lu", next_child_num_++);

  fuchsia::component::RealmSyncPtr realm;
  if (zx_status_t status = service_directory_->Connect(realm.NewRequest()); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to connect to realm service";
    return;
  }

  fuchsia::component::ControllerSyncPtr controller;
  {
    fuchsia::component::decl::CollectionRef collection{
        .name = std::string(kShellCollection),
    };
    fuchsia::component::decl::Child decl;
    decl.set_name(child_name);
    decl.set_url("#meta/sshd.cm");
    decl.set_startup(fuchsia::component::decl::StartupMode::LAZY);

    fuchsia::component::CreateChildArgs args;
    args.set_controller(controller.NewRequest());

    fuchsia::component::Realm_CreateChild_Result result;
    if (zx_status_t status =
            realm->CreateChild(collection, std::move(decl), std::move(args), &result);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to create sshd child";
      return;
    }

    if (result.is_err()) {
      FX_LOGS(ERROR) << "Failed to create sshd child: " << static_cast<uint32_t>(result.err());
      return;
    }
  }

  fuchsia::component::ExecutionControllerPtr& execution_controller = controllers_.emplace_back();
  auto remove_controller_on_error = fit::defer([this]() { controllers_.pop_back(); });

  fit::callback<void(zx_status_t)> on_stop_cb = [this, ptr = execution_controller.get(),
                                                 realm = std::move(realm),
                                                 child_name](zx_status_t status) {
    if (status != ZX_OK) {
      FX_PLOGS(WARNING, status) << "sshd component stopped with error";
    }

    // Remove the controller.
    auto i = std::find_if(controllers_.begin(), controllers_.end(),
                          [ptr](const fuchsia::component::ExecutionControllerPtr& controller) {
                            return controller.get() == ptr;
                          });
    if (i != controllers_.end()) {
      controllers_.erase(i);
    };

    // Destroy the component.
    fuchsia::component::decl::ChildRef child_ref = {
        .name = child_name,
        .collection = std::string(kShellCollection),
    };
    fuchsia::component::Realm_DestroyChild_Result result;
    if (zx_status_t status = realm->DestroyChild(std::move(child_ref), &result); status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to destroy sshd child";
      return;
    }

    if (result.is_err()) {
      FX_LOGS(ERROR) << "Failed to destroy sshd child: " << static_cast<uint32_t>(result.err());
      return;
    }
  };

  execution_controller.events().OnStop =
      [on_stop_cb = on_stop_cb.share()](fuchsia::component::StoppedPayload payload) mutable {
        zx_status_t status = ZX_OK;
        if (payload.has_status()) {
          status = payload.status();
        }
        on_stop_cb(status);
      };
  execution_controller.set_error_handler(
      [on_stop_cb = std::move(on_stop_cb)](zx_status_t status) mutable { on_stop_cb(status); });

  // Pass the connection fd as stdin and stdout handles to the sshd component.
  fuchsia::component::StartChildArgs start_args;
  for (int fd : {STDIN_FILENO, STDOUT_FILENO}) {
    zx::handle conn_handle;
    if (zx_status_t status = fdio_fd_clone(conn.get(), conn_handle.reset_and_get_address());
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to clone connection file descriptor " << conn.get();
      return;
    }
    start_args.mutable_numbered_handles()->push_back(
        fuchsia::process::HandleInfo{.handle = std::move(conn_handle), .id = PA_HND(PA_FD, fd)});
  }

  fuchsia::component::Controller_Start_Result result;
  if (zx_status_t status = controller->Start(std::move(start_args),
                                             execution_controller.NewRequest(dispatcher_), &result);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to start sshd child";
    return;
  }

  if (result.is_err()) {
    FX_LOGS(ERROR) << "Failed to start sshd child: " << static_cast<uint32_t>(result.err());
    return;
  }

  remove_controller_on_error.cancel();
}

}  // namespace sshd_host
