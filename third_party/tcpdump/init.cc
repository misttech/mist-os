// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.posix.socket.packet/cpp/wire.h>
#include <fidl/fuchsia.sys2/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/vfs/cpp/composed_service_dir.h>
#include <lib/zx/channel.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <filesystem>

namespace {

constexpr char kRealmQueryPath[] = "/svc/fuchsia.sys2.RealmQuery.root";
constexpr char kNetstackMoniker[] = "./core/network/netstack";
constexpr char kRootDirectory[] = "/";
constexpr char kServiceDirectory[] = "/svc";
constexpr const char* kPacketSocketProviderName =
    fidl::DiscoverableProtocolName<fuchsia_posix_socket_packet::Provider>;

}  // namespace

// Attempts to make a packet socket provider available to this program if not
// already available.
//
// The packet socket provider exposed by the core realm's netstack is used if it
// is available.
__attribute__((constructor)) void init_packet_socket_provider() {
  std::filesystem::path svc_dir_path(kServiceDirectory);

  if (std::filesystem::exists(svc_dir_path / kPacketSocketProviderName)) {
    // Packet socket provider is already available.
    return;
  }

  static async::Loop composed_dir_loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  // Replace the default service directory with our composed service directory
  // to make packet socket provider available to the program and start serving
  // requests to the composed service directory.
  {
    fdio_ns_t* ns;
    {
      zx_status_t status = fdio_ns_get_installed(&ns);
      ZX_ASSERT_MSG(status == ZX_OK, "fdio_ns_get_installed(_): %s", zx_status_get_string(status));
    }

    constexpr fuchsia::io::OpenFlags kServeFlags(fuchsia::io::OpenFlags::DIRECTORY);

    auto bind_to_ns = [ns](const char* path, vfs::ComposedServiceDir& composed_dir) {
      zx::channel client, server;
      {
        zx_status_t status = zx::channel::create(0, &client, &server);
        ZX_ASSERT_MSG(status == ZX_OK, "zx::channel::create(0, _, _): %s",
                      zx_status_get_string(status));
      }
      {
        zx_status_t status = fdio_ns_bind(ns, path, client.release());
        ZX_ASSERT_MSG(status == ZX_OK, "fdio_ns_bind(_, %s, _): %s", path,
                      zx_status_get_string(status));
      }
      {
        zx_status_t status =
            composed_dir.Serve(kServeFlags, std::move(server), composed_dir_loop.dispatcher());
        ZX_ASSERT_MSG(status == ZX_OK, "composed_dir.Serve(0x%x, _, _): %s", kServeFlags,
                      zx_status_get_string(status));
      }
    };

    static vfs::ComposedServiceDir composed_svc_dir;

    // Our composed service directory should be a superset of the default service
    // directory.
    {
      zx::result original_svc_dir = component::OpenServiceRoot();
      switch (zx_status_t status = original_svc_dir.status_value(); status) {
        case ZX_OK:
          break;
        case ZX_ERR_NOT_FOUND:
          // Environment didn't populate a service directory for us to use as a
          // fallback.
          return;
        default:
          ZX_PANIC("component::OpenServiceRoot(): %s", zx_status_get_string(status));
      }
      // TODO(https://fxbug.dev/72980): Avoid this type-unsafe conversion.
      composed_svc_dir.set_fallback(
          fidl::InterfaceHandle<fuchsia::io::Directory>(original_svc_dir->TakeChannel()));
    }

    // Add the packet socket provider service to our composed service directory
    // by reaching into netstack's exposed directory.
    {
      zx::result realm_query = component::Connect<fuchsia_sys2::RealmQuery>(kRealmQueryPath);
      ZX_ASSERT_MSG(realm_query.is_ok(), "Failed to connect to %s: %s", kRealmQueryPath,
                    realm_query.status_string());
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Node>();
      ZX_ASSERT_MSG(endpoints.is_ok(), "Failed to create endpoints: %s", endpoints.status_string());
      fidl::WireResult open_dir =
          fidl::WireCall(realm_query.value())
              ->Open(kNetstackMoniker, fuchsia_sys2::OpenDirType::kExposedDir,
                     fuchsia_io::OpenFlags::kRightReadable, fuchsia_io::ModeType(), ".",
                     std::move(endpoints->server));
      ZX_ASSERT_MSG(open_dir.ok(), "Failed to open %s: %s", kNetstackMoniker,
                    open_dir.status_string());

      composed_svc_dir.AddService(
          kPacketSocketProviderName,
          std::make_unique<vfs::Service>(
              [netstack_exposed_dir =
                   fidl::ClientEnd<fuchsia_io::Directory>(endpoints->client.TakeChannel())](
                  zx::channel request, async_dispatcher_t* dispatcher) mutable {
                zx::result result = component::ConnectAt(
                    netstack_exposed_dir.borrow(),
                    fidl::ServerEnd<fuchsia_posix_socket_packet::Provider>(std::move(request)));
                ZX_ASSERT_MSG(result.is_ok(), "Failed to connect to packet socker provider: %s",
                              result.status_string());
              }));
    }

    // Attempt to unbind the service directory from the namespace so we can
    // replace it.
    switch (zx_status_t status = fdio_ns_unbind(ns, svc_dir_path.c_str()); status) {
      // We get |ZX_OK| when the namespace the service directory is a mount
      // point.
      case ZX_OK:
        bind_to_ns(svc_dir_path.c_str(), composed_svc_dir);
        break;
      // We get |ZX_ERR_BAD_PATH| when the service directory isn't a mount point
      // in the namespace (process launched with delayed directories after
      // https://fuchsia.googlesource.com/fuchsia/+/82ad8d81396d5659515e830a7364cf33b1605b69).
      case ZX_ERR_BAD_PATH: {
        std::filesystem::path root(kRootDirectory);
        static vfs::ComposedServiceDir composed_root_dir;
        {
          zx::channel client, server;
          {
            zx_status_t status = zx::channel::create(0, &client, &server);
            ZX_ASSERT_MSG(status == ZX_OK, "zx::channel::create(0, _, _): %s",
                          zx_status_get_string(status));
          }
          {
            zx_status_t status = fdio_service_connect(root.c_str(), server.release());
            ZX_ASSERT_MSG(status == ZX_OK, "fdio_service_connect(%s, _): %s", root.c_str(),
                          zx_status_get_string(status));
          }
          composed_root_dir.set_fallback(
              fidl::InterfaceHandle<fuchsia::io::Directory>(std::move(client)));
        }

        composed_root_dir.AddService(
            svc_dir_path.filename().c_str(),
            std::make_unique<vfs::Service>(
                [](zx::channel request, async_dispatcher_t* dispatcher) mutable {
                  zx_status_t status =
                      composed_svc_dir.Serve(kServeFlags, std::move(request), dispatcher);
                  ZX_ASSERT_MSG(status == ZX_OK, "composed_svc_dir.Serve(0x%x, _, _): %s",
                                kServeFlags, zx_status_get_string(status));
                }));

        {
          zx_status_t status = fdio_ns_unbind(ns, root.c_str());
          ZX_ASSERT_MSG(status == ZX_OK, "fdio_ns_unbind(_, %s): %s", root.c_str(),
                        zx_status_get_string(status));
        }

        bind_to_ns(root.c_str(), composed_root_dir);
      } break;
      default:
        ZX_PANIC("fdio_ns_unbind(_, %s): %s", svc_dir_path.c_str(), zx_status_get_string(status));
    }
  }

  zx_status_t status = composed_dir_loop.StartThread();
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to start async loop thread: %s",
                zx_status_get_string(status));
}
