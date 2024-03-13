// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/sshd-host/service.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.process/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/fd.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
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
    fidl::SyncClient<fuchsia_boot::Items>& boot_items) {
  auto result =
      boot_items->GetBootloaderFile({{.filename = std::string(kAuthorizedKeysBootloaderFileName)}});

  if (result.is_error()) {
    FX_PLOGS(ERROR, result.error_value().status())
        << "Provisioning keys from boot item: GetBootloaderFile failed";
    return result.error_value().status();
  }
  zx::vmo vmo = std::move(result->payload());

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

Service::Service(async_dispatcher_t* dispatcher, uint16_t port)
    : dispatcher_(dispatcher),
      sock_(fbl::unique_fd(socket(AF_INET6, SOCK_STREAM, IPPROTO_TCP))),
      waiter_(dispatcher) {
  if (!sock_.is_valid()) {
    FX_LOGS(FATAL) << "Failed to create socket: " << strerror(errno);
  }
  sockaddr_storage addr;
  *reinterpret_cast<struct sockaddr_in6*>(&addr) = sockaddr_in6{
      .sin6_family = AF_INET6,
      .sin6_port = htons(port),
      .sin6_addr = in6addr_any,
  };
  if (bind(sock_.get(), reinterpret_cast<const sockaddr*>(&addr), sizeof addr) < 0) {
    FX_LOGS(FATAL) << "Failed to bind to " << port << ": " << strerror(errno);
  }

  FX_SLOG(INFO, "listen() for inbound SSH connections", FX_KV("port", (int)port));
  if (listen(sock_.get(), 10) < 0) {
    FX_LOGS(FATAL) << "Failed to listen: " << strerror(errno);
  }

  Wait();
}

Service::~Service() = default;

void Service::Wait() {
  FX_SLOG(DEBUG, "Waiting for next connection");

  waiter_.Wait(
      [this](zx_status_t status, uint32_t /*events*/) {
        if (status != ZX_OK) {
          FX_PLOGS(FATAL, status) << "Failed to wait on socket";
        }

        struct sockaddr_storage peer_addr {};
        socklen_t peer_addr_len = sizeof(peer_addr);
        fbl::unique_fd conn(
            accept(sock_.get(), reinterpret_cast<struct sockaddr*>(&peer_addr), &peer_addr_len));
        if (!conn.is_valid()) {
          if (errno == EPIPE) {
            FX_LOGS(FATAL) << "The netstack died. Terminating.";
          } else {
            FX_LOGS(ERROR) << "Failed to accept: " << strerror(errno);
            // Wait for another connection.
            Wait();
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
          peer_name = fxl::StringPrintf("[%s]:%s", host, port);
        } else {
          FX_LOGS(WARNING)
              << "Error from getnameinfo(.., NI_NUMERICHOST | NI_NUMERICSERV) for peer address: "
              << gai_strerror(res);
        }
        FX_SLOG(INFO, "Accepted connection", FX_KV("remote", peer_name.c_str()));

        Launch(std::move(conn));
        Wait();
      },
      sock_.get(), POLLIN);
}

void Service::Launch(fbl::unique_fd conn) {
  uint64_t child_num = next_child_num_++;
  std::string child_name = fxl::StringPrintf("sshd-%lu", child_num);

  auto realm_client_end = component::Connect<fuchsia_component::Realm>();
  if (realm_client_end.is_error()) {
    FX_PLOGS(ERROR, realm_client_end.status_value()) << "Failed to connect to realm service";
    return;
  }

  fidl::SyncClient<fuchsia_component::Realm> realm{std::move(*realm_client_end)};

  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_component::Controller>();
  if (controller_endpoints.is_error()) {
    FX_PLOGS(ERROR, controller_endpoints.status_value())
        << "Failed to connect to create controller endpoints";
    return;
  }

  fidl::SyncClient<fuchsia_component::Controller> controller{
      std::move(controller_endpoints->client)};
  {
    fuchsia_component_decl::CollectionRef collection{{
        .name = std::string(kShellCollection),
    }};
    fuchsia_component_decl::Child decl{{.name = child_name,
                                        .url = "#meta/sshd.cm",
                                        .startup = fuchsia_component_decl::StartupMode::kLazy}};

    fuchsia_component::CreateChildArgs args{
        {.controller = std::move(controller_endpoints->server)}};

    auto result = realm->CreateChild(
        {{.collection = collection, .decl = std::move(decl), .args = std::move(args)}});
    if (result.is_error()) {
      FX_LOGS(ERROR) << "Failed to create sshd child: " << result.error_value().FormatDescription();
      return;
    }
  }

  auto execution_controller_endpoints =
      fidl::CreateEndpoints<fuchsia_component::ExecutionController>();
  if (execution_controller_endpoints.is_error()) {
    FX_LOGS(ERROR) << "Failed to create execution controller endpoints: "
                   << execution_controller_endpoints.status_string();
    return;
  }

  controllers_.emplace(std::piecewise_construct, std::forward_as_tuple(child_num),
                       std::forward_as_tuple(this, child_num, std::move(child_name),
                                             std::move(execution_controller_endpoints->client),
                                             dispatcher_, std::move(realm)));
  auto remove_controller_on_error =
      fit::defer([this, child_num]() { controllers_.erase(child_num); });

  // Pass the connection fd as stdin and stdout handles to the sshd component.
  std::vector<fuchsia_process::HandleInfo> numbered_handles;
  for (int fd : {STDIN_FILENO, STDOUT_FILENO}) {
    zx::handle conn_handle;
    if (zx_status_t status = fdio_fd_clone(conn.get(), conn_handle.reset_and_get_address());
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to clone connection file descriptor " << conn.get();
      return;
    }
    numbered_handles.push_back(
        fuchsia_process::HandleInfo{{.handle = std::move(conn_handle), .id = PA_HND(PA_FD, fd)}});
  }

  auto result = controller->Start(
      {{.args = {{
            .numbered_handles = std::move(numbered_handles),
            .namespace_entries = {},
        }},
        .execution_controller = std::move(execution_controller_endpoints->server)}});

  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to start sshd child: " << result.error_value().FormatDescription();
    return;
  }

  remove_controller_on_error.cancel();
}

void Service::OnStop(zx_status_t status, Controller* ptr) {
  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "sshd component stopped with error";
  }

  // Destroy the component.
  auto result = ptr->realm_->DestroyChild({{.child = {{
                                                .name = ptr->child_name_,
                                                .collection = std::string(kShellCollection),

                                            }}}});
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to destroy sshd child: " << result.error_value().FormatDescription();
  }

  // Remove the controller.
  controllers_.erase(ptr->child_num_);
}

}  // namespace sshd_host
