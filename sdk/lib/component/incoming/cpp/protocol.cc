// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>

namespace component {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> OpenServiceRoot(std::string_view path) {
  // NOTE: Although connecting to a protocol does not require rights, many service directory clients
  // assume a basic set of operations (e.g. enumerating service instances).
  //
  // The fuchsia.io/PERM_READABLE constant (r* in component manifests) capture these rights.
  constexpr uint64_t kServiceRootFlags =
      uint64_t{fuchsia_io::wire::kPermReadable | fuchsia_io::Flags::kProtocolDirectory};
  auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  return zx::make_result(
      fdio_open3(std::string(path).c_str(), kServiceRootFlags, server.TakeChannel().release()),
      std::move(client));
}

}  // namespace component
