// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/vfs.h>
#include <zircon/status.h>

#include <string>
#include <string_view>

#include <runtests-utils/service-proxy-dir.h>

namespace fio = fuchsia_io;

namespace runtests {

ServiceProxyDir::ServiceProxyDir(fidl::ClientEnd<fio::Directory> proxy_dir)
    : proxy_dir_(std::move(proxy_dir)) {}

void ServiceProxyDir::AddEntry(std::string name, fbl::RefPtr<fs::Vnode> node) {
  std::lock_guard lock(lock_);
  entries_[std::move(name)] = std::move(node);
}

zx::result<fs::VnodeAttributes> ServiceProxyDir::GetAttributes() const {
  return zx::ok(fs::VnodeAttributes{
      .mode = V_TYPE_DIR | V_IRUSR,
  });
}

fuchsia_io::NodeProtocolKinds ServiceProxyDir::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kDirectory;
}

zx_status_t ServiceProxyDir::Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) {
  auto entry_name = std::string(name.data(), name.length());

  std::lock_guard lock(lock_);
  auto entry = entries_.find(entry_name);
  if (entry != entries_.end()) {
    *out = entry->second;
    return ZX_OK;
  }
  // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
  entries_.emplace(
      entry_name, *out = fbl::MakeRefCounted<fs::Service>([this, entry_name](
                                                              fidl::ServerEnd<fio::Node> request) {
        return fidl::WireCall(proxy_dir_)
            ->DeprecatedOpen({}, {}, fidl::StringView::FromExternal(entry_name), std::move(request))
            .status();
      }));

  return ZX_OK;
}

}  // namespace runtests
