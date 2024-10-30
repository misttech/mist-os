// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>

#include <android/system/microfuchsia/vm_service/IMicrofuchsia.h>
#include <binder/Binder.h>
#include <binder/RpcSession.h>
#include <binder/unique_fd.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"

namespace {

using IMicrofuchsia = android::system::microfuchsia::vm_service::IMicrofuchsia;

int BinderProxyHostTool() {
  constexpr int kLoopbackPort = 5680;

  auto session = android::RpcSession::make();
  session->setMaxIncomingThreads(1);

  auto status = session->setupPreconnectedClient({}, [] {
    FX_LOGS(TRACE) << "preconnected client callback invoked";
    auto socket_fd = android::binder::unique_fd(socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0));
    if (!socket_fd.ok()) {
      FX_LOGS(ERROR) << "socket failed: " << errno << ": " << strerror(errno);
      return android::binder::unique_fd();
    }

    int no_delay = 1;
    int rv = setsockopt(socket_fd.get(), IPPROTO_TCP, TCP_NODELAY, &no_delay, sizeof(no_delay));
    if (rv < 0) {
      FX_LOGS(ERROR) << "setsockopt failed: " << errno << ": " << strerror(errno);
      return android::binder::unique_fd();
    }

    sockaddr_in loopback_addr{
        .sin_family = AF_INET,
        .sin_port = htons(kLoopbackPort),
        .sin_addr =
            {
                .s_addr = htonl(INADDR_LOOPBACK),
            },
    };

    for (int tries = 0; tries < 5; ++tries) {
      FX_LOGS(TRACE) << "connecting";
      rv = connect(socket_fd.get(), reinterpret_cast<struct sockaddr*>(&loopback_addr),
                   sizeof(loopback_addr));
      if (rv == 0) {
        break;
      }
      // If we can't connect, sleep for a bit to let the target port come up.
      sleep(2);
    }
    if (rv != 0) {
      FX_LOGS(ERROR) << "Could not connect: " << errno << ": " << strerror(errno);
      return android::binder::unique_fd();
    }

    return socket_fd;
  });

  if (status != android::OK) {
    FX_LOGS(ERROR) << "setupPreconnectedClient failed: " << android::statusToString(status);
    return EXIT_FAILURE;
  }

  auto root_object = session->getRootObject();
  status = root_object->pingBinder();
  if (status != android::OK) {
    FX_LOGS(ERROR) << "pingBinder failed: " << android::statusToString(status);
    return EXIT_FAILURE;
  }

  FX_LOGS(INFO) << "binder ping received";

  return EXIT_SUCCESS;
}

}  // namespace

int main(int argc, char** argv) {
  fxl::SetLogSettingsFromCommandLine(fxl::CommandLineFromArgcArgv(argc, argv));
  return BinderProxyHostTool();
}