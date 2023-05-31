// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component.resolution/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/fdio/directory.h>
#include <lib/svc/dir.h>
#include <lib/svc/outgoing.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>

#include <fstream>
#include <memory>
#include <vector>

#include "src/lib/storage/vfs/cpp/remote_dir.h"

namespace {

class FakeBootResolver final : public fidl::WireServer<fuchsia_component_resolution::Resolver> {
 public:
  void SetPkgDir(fbl::RefPtr<fs::RemoteDir> pkg_dir) { pkg_dir_ = std::move(pkg_dir); }

 private:
  void Resolve(ResolveRequestView request, ResolveCompleter::Sync& completer) override {
    std::string_view kPrefix = "fuchsia-boot:///";
    std::string_view relative_path = request->component_url.get();
    if (!cpp20::starts_with(relative_path, kPrefix)) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInvalidArgs);
      return;
    }
    relative_path.remove_prefix(kPrefix.size() + 1);

    // driver_host2 is supposed to be in a bootfs package, but it's actually in the same package
    // as the driver test realm for test scenarios.
    // TODO(http://fxbug.dev/126086): Include driver_host2 as a subpackage to avoid this hack.
    std::string_view kDriverHostPrefix = "river_host2";
    if (cpp20::starts_with(relative_path, kDriverHostPrefix)) {
      relative_path.remove_prefix(kDriverHostPrefix.size() + 1);
    }

    auto file = fidl::CreateEndpoints<fuchsia_io::File>();
    if (file.is_error()) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInternal);
      return;
    }
    zx_status_t status =
        fdio_open_at(pkg_dir_->client_end().channel()->get(), std::string(relative_path).data(),
                     static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kRightReadable),
                     file->server.channel().release());
    if (status != ZX_OK) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInternal);
      return;
    }
    fidl::WireResult result =
        fidl::WireCall(file->client)->GetBackingMemory(fuchsia_io::wire::VmoFlags::kRead);
    if (!result.ok()) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kIo);
      return;
    }
    auto& response = result.value();
    if (response.is_error()) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kIo);
      return;
    }
    zx::vmo& vmo = response.value()->vmo;
    uint64_t size;
    status = vmo.get_prop_content_size(&size);
    if (status != ZX_OK) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kIo);
    }

    // Not all boot components resolved by this resolver are packaged (e.g. root.cm) so they do not
    // have an ABI revision. As a workaround, apply the ABI revision of this package so components
    // can pass runtime ABI compatibility checks during testing.
    std::ifstream abi_revision_file("/pkg/meta/fuchsia.abi/abi-revision");
    if (!abi_revision_file) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kIo);
      return;
    }
    uint64_t abi_revision;
    abi_revision_file.read(reinterpret_cast<char*>(&abi_revision), sizeof(abi_revision));
    if (!abi_revision_file) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kIo);
      return;
    }
    abi_revision_file.close();

    zx::result directory = component::Clone(pkg_dir_->client_end());
    if (directory.is_error()) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInternal);
      return;
    }

    fidl::Arena arena;
    auto package = fuchsia_component_resolution::wire::Package::Builder(arena)
                       .url(kPrefix)
                       .directory(std::move(directory.value()))
                       .Build();

    auto component = fuchsia_component_resolution::wire::Component::Builder(arena)
                         .url(request->component_url)
                         .abi_revision(abi_revision)
                         .decl(fuchsia_mem::wire::Data::WithBuffer(arena,
                                                                   fuchsia_mem::wire::Buffer{
                                                                       .vmo = std::move(vmo),
                                                                       .size = size,
                                                                   }))
                         .package(package)
                         .Build();
    completer.ReplySuccess(component);
  }

  void ResolveWithContext(ResolveWithContextRequestView request,
                          ResolveWithContextCompleter::Sync& completer) override {
    FX_SLOG(ERROR, "FakeBootResolver does not currently support ResolveWithContext");
    completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInvalidArgs);
  }

  fbl::RefPtr<fs::RemoteDir> pkg_dir_;
};

class DriverTestRealm final {
 public:
  DriverTestRealm(svc::Outgoing* outgoing, async::Loop* loop) : outgoing_(outgoing), loop_(loop) {}

  static zx::result<std::unique_ptr<DriverTestRealm>> Create(svc::Outgoing* outgoing,
                                                             async::Loop* loop) {
    auto realm = std::make_unique<DriverTestRealm>(outgoing, loop);
    zx_status_t status = realm->Initialize();
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(realm));
  }

 private:
  zx_status_t Initialize() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.status_value();
    }
    zx_status_t status =
        fdio_open("/boot",
                  static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory |
                                        fuchsia_io::wire::OpenFlags::kRightReadable |
                                        fuchsia_io::wire::OpenFlags::kRightExecutable),
                  endpoints->server.channel().release());
    if (status != ZX_OK) {
      return status;
      ;
    }
    auto remote_dir = fbl::MakeRefCounted<fs::RemoteDir>(std::move(endpoints->client));
    boot_resolver_.SetPkgDir(remote_dir);

    auto service_callback =
        [this](fidl::ServerEnd<fuchsia_component_resolution::Resolver> request) {
          fidl::BindServer(loop_->dispatcher(), std::move(request), &boot_resolver_);
          return ZX_OK;
        };
    status = outgoing_->svc_dir()->AddEntry(
        fidl::DiscoverableProtocolName<fuchsia_component_resolution::Resolver>,
        fbl::MakeRefCounted<fs::Service>(std::move(service_callback)));
    if (status != ZX_OK) {
      return ZX_ERR_INTERNAL;
    }

    return ZX_OK;
  }

  svc::Outgoing* outgoing_;
  async::Loop* loop_;

  FakeBootResolver boot_resolver_;
};

}  // namespace

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  svc::Outgoing outgoing(loop.dispatcher());
  zx_status_t status = outgoing.ServeFromStartupInfo();
  if (status != ZX_OK) {
    return status;
  }
  auto realm = DriverTestRealm::Create(&outgoing, &loop);
  if (realm.status_value() != ZX_OK) {
    return realm.status_value();
  }

  loop.Run();
  return 0;
}
