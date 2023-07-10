// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component.resolution/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/directory.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>

#include <fstream>

namespace {

class FakeComponentResolver final
    : public fidl::WireServer<fuchsia_component_resolution::Resolver> {
 public:
  explicit FakeComponentResolver(fidl::ClientEnd<fuchsia_io::Directory> boot_dir,
                                 fidl::ClientEnd<fuchsia_io::Directory> pkg_dir)
      : boot_dir_(std::move(boot_dir)), pkg_dir_(std::move(pkg_dir)) {}

 private:
  void Resolve(ResolveRequestView request, ResolveCompleter::Sync& completer) override {
    std::string_view kBootPrefix = "fuchsia-boot:///";
    std::string_view kPkgPrefix = "fuchsia-pkg://fuchsia.com/";
    std::string_view relative_path = request->component_url.get();

    std::string pkg_url;
    bool is_boot = false;

    if (cpp20::starts_with(relative_path, kBootPrefix)) {
      is_boot = true;
      relative_path.remove_prefix(kBootPrefix.size() + 1);
      pkg_url = kBootPrefix;

      // driver_host2 is supposed to be in a bootfs package, but it's actually in the same package
      // as the driver test realm for test scenarios.
      // TODO(http://fxbug.dev/126086): Include driver_host2 as a subpackage to avoid this hack.
      std::string_view kDriverHostPrefix = "river_host2";
      if (cpp20::starts_with(relative_path, kDriverHostPrefix)) {
        relative_path.remove_prefix(kDriverHostPrefix.size() + 1);
      }
    } else if (cpp20::starts_with(relative_path, kPkgPrefix)) {
      relative_path.remove_prefix(kPkgPrefix.size());
      size_t pos = relative_path.find('#');
      std::string pkg_name = std::string(relative_path.substr(0, pos));
      relative_path.remove_prefix(pos + 1);
      pkg_url = std::string(kPkgPrefix) + pkg_name;
    } else {
      FX_SLOG(ERROR, "FakeComponentResolver request not supported.",
              KV("url", std::string(relative_path).c_str()));
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInvalidArgs);
      return;
    }

    auto file = fidl::CreateEndpoints<fuchsia_io::File>();
    if (file.is_error()) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInternal);
      return;
    }

    zx_handle_t dir = pkg_dir_.channel().get();
    if (is_boot) {
      dir = boot_dir_.channel().get();
    }

    zx_status_t status =
        fdio_open_at(dir, std::string(relative_path).data(),
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

    // Not all boot components resolved by this resolver are packaged (e.g. root.cm) so they do
    // not have an ABI revision. As a workaround, apply the ABI revision of this package so
    // components can pass runtime ABI compatibility checks during testing.
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

    zx::result<fidl::ClientEnd<fuchsia_io::Directory>> dir_clone_result;
    if (is_boot) {
      dir_clone_result = component::Clone(boot_dir_);
    } else {
      dir_clone_result = component::Clone(pkg_dir_);
    }

    if (dir_clone_result.is_error()) {
      completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInternal);
      return;
    }

    fidl::Arena arena;
    auto package = fuchsia_component_resolution::wire::Package::Builder(arena)
                       .url(pkg_url)
                       .directory(std::move(dir_clone_result.value()))
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
    FX_SLOG(ERROR, "FakeComponentResolver does not currently support ResolveWithContext");
    completer.ReplyError(fuchsia_component_resolution::wire::ResolverError::kInvalidArgs);
  }

  fidl::ClientEnd<fuchsia_io::Directory> boot_dir_;
  fidl::ClientEnd<fuchsia_io::Directory> pkg_dir_;
};

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> ConnectDir(const char* dir) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  zx_status_t status =
      fdio_open(dir,
                static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory |
                                      fuchsia_io::wire::OpenFlags::kRightReadable |
                                      fuchsia_io::wire::OpenFlags::kRightExecutable),
                endpoints->server.channel().release());
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(endpoints->client));
}

}  // namespace

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  component::OutgoingDirectory outgoing(loop.dispatcher());
  zx::result<> serve_result = outgoing.ServeFromStartupInfo();
  if (serve_result.is_error()) {
    return serve_result.status_value();
  }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> connect_boot_result = ConnectDir("/boot");
  if (connect_boot_result.is_error()) {
    return connect_boot_result.status_value();
  }

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> connect_pkg_result = ConnectDir("/pkg");
  if (connect_pkg_result.is_error()) {
    return connect_pkg_result.status_value();
  }

  zx::result<> add_protocol_result = outgoing.AddProtocol<fuchsia_component_resolution::Resolver>(
      std::make_unique<FakeComponentResolver>(std::move(connect_boot_result.value()),
                                              std::move(connect_pkg_result.value())));
  if (add_protocol_result.is_error()) {
    return add_protocol_result.status_value();
  }

  loop.Run();
  return 0;
}
