// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/zx/channel.h>

namespace sys {
namespace {

zx::channel OpenServiceRoot() {
  fdio_flat_namespace_t* out;
  if (zx_status_t status = fdio_ns_export_root(&out); status != ZX_OK) {
    return {};
  }
  auto deferred = fit::defer([out]() { fdio_ns_free_flat_ns(out); });
  const fdio_flat_namespace_t& ns = *out;

  // Look for /svc in the namespace. If that fails, try to connect directly via
  // the entry at /. Since the namespace can't contain paths where one shadows
  // another, at most one of these entries exists.
  // TODO(https://fxbug.dev/101092): Replace this with a call to
  // fdio_service_connect("/svc", ...) once nothing is trying to open /svc with
  // rights.
  for (size_t i = 0; i < ns.count; ++i) {
    std::string_view path{ns.path[i]};
    if (path == "/svc") {
      return zx::channel{std::exchange(ns.handle[i], ZX_HANDLE_INVALID)};
    }
    if (path == "/") {
      zx::channel request, service_root;
      if (zx::channel::create(0, &request, &service_root) != ZX_OK) {
        return {};
      }
      if (fdio_service_connect_at(ns.handle[i], "/svc", request.release()) != ZX_OK) {
        return {};
      }
      return service_root;
    }
  }

  return {};
}

}  // namespace

ServiceDirectory::ServiceDirectory(zx::channel directory)
    : ServiceDirectory(fidl::InterfaceHandle<fuchsia::io::Directory>(std::move(directory))) {}

ServiceDirectory::ServiceDirectory(fidl::InterfaceHandle<fuchsia::io::Directory> directory)
    : directory_(directory.BindSync()) {}

ServiceDirectory::~ServiceDirectory() = default;

std::shared_ptr<ServiceDirectory> ServiceDirectory::CreateFromNamespace() {
  return std::make_shared<ServiceDirectory>(OpenServiceRoot());
}

std::shared_ptr<ServiceDirectory> ServiceDirectory::CreateWithRequest(zx::channel* out_request) {
  zx::channel directory;
  // no need to check status, even if this fails, service directory would be
  // backed by invalid channel and Connect will return correct error.
  zx::channel::create(0, &directory, out_request);

  return std::make_shared<ServiceDirectory>(ServiceDirectory(std::move(directory)));
}

std::shared_ptr<ServiceDirectory> ServiceDirectory::CreateWithRequest(
    fidl::InterfaceRequest<fuchsia::io::Directory>* out_request) {
  zx::channel request;
  auto directory = CreateWithRequest(&request);
  out_request->set_channel(std::move(request));
  return directory;
}

zx_status_t ServiceDirectory::Connect(const std::string& interface_name,
                                      zx::channel request) const {
  return fdio_service_connect_at(directory_.unowned_channel()->get(), interface_name.c_str(),
                                 request.release());
}

fidl::InterfaceHandle<fuchsia::io::Directory> ServiceDirectory::CloneChannel() const {
  fidl::InterfaceHandle<fuchsia::io::Directory> dir;
  CloneChannel(dir.NewRequest());
  return dir;
}

zx_status_t ServiceDirectory::CloneChannel(
    fidl::InterfaceRequest<fuchsia::io::Directory> dir) const {
  if (!directory_.is_bound()) {
    return ZX_ERR_BAD_HANDLE;
  }
  return directory_->Clone(fuchsia::io::OpenFlags::CLONE_SAME_RIGHTS,
                           fidl::InterfaceRequest<fuchsia::io::Node>(dir.TakeChannel()));
}

}  // namespace sys
