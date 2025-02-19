// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/sys/cpp/outgoing_directory.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/service.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include <utility>

namespace {

// Adds a new empty directory |name| to |dir| and returns pointer to new
// directory. Will fail silently if directory with that name already exists.
vfs::PseudoDir* AddNewEmptyDirectory(vfs::PseudoDir* dir, std::string name) {
  auto subdir = std::make_unique<vfs::PseudoDir>();
  auto ptr = subdir.get();
  dir->AddEntry(std::move(name), std::move(subdir));
  return ptr;
}

vfs::PseudoDir* GetOrCreateDirectory(vfs::PseudoDir* dir, std::string name) {
  vfs::Node* node;
  zx_status_t status = dir->Lookup(name, &node);
  if (status != ZX_OK) {
    return AddNewEmptyDirectory(dir, std::move(name));
  }
  return static_cast<vfs::PseudoDir*>(node);
}

}  // namespace

namespace sys {

OutgoingDirectory::OutgoingDirectory()
    : root_(std::make_unique<vfs::PseudoDir>()),
      svc_(AddNewEmptyDirectory(root_.get(), "svc")),
      debug_(AddNewEmptyDirectory(root_.get(), "debug")) {}

OutgoingDirectory::~OutgoingDirectory() = default;

OutgoingDirectory::OutgoingDirectory(OutgoingDirectory&& other) noexcept
    : root_(std::move(other.root_)), svc_(other.svc_), debug_(other.debug_) {
  other.svc_ = nullptr;
  other.debug_ = nullptr;
}

OutgoingDirectory& OutgoingDirectory::operator=(OutgoingDirectory&& other) noexcept {
  root_ = std::move(other.root_);
  svc_ = other.svc_;
  debug_ = other.debug_;

  other.svc_ = nullptr;
  other.debug_ = nullptr;
  return *this;
}

zx_status_t OutgoingDirectory::Serve(
    fidl::InterfaceRequest<fuchsia::io::Directory> directory_request,
    async_dispatcher_t* dispatcher) {
  if (!directory_request.is_valid()) {
    return ZX_ERR_BAD_HANDLE;
  }
  return root_->Serve(fuchsia_io::wire::kPermReadable | fuchsia_io::wire::kPermWritable,
                      fidl::ServerEnd<fuchsia_io::Directory>(directory_request.TakeChannel()),
                      dispatcher);
}

zx_status_t OutgoingDirectory::ServeFromStartupInfo(async_dispatcher_t* dispatcher) {
  zx::channel directory_request{zx_take_startup_handle(PA_DIRECTORY_REQUEST)};
  return Serve(fidl::InterfaceRequest<fuchsia::io::Directory>(std::move(directory_request)),
               dispatcher);
}

vfs::PseudoDir* OutgoingDirectory::GetOrCreateDirectory(const std::string& name) {
  return ::GetOrCreateDirectory(root_.get(), name);
}

zx_status_t OutgoingDirectory::AddPublicService(std::unique_ptr<vfs::Service> service,
                                                std::string service_name) const {
  return svc_->AddEntry(std::move(service_name), std::move(service));
}

zx_status_t OutgoingDirectory::AddPublicService(Connector connector,
                                                std::string service_name) const {
  return AddPublicService(std::make_unique<vfs::Service>(std::move(connector)),
                          std::move(service_name));
}

zx_status_t OutgoingDirectory::RemovePublicService(const std::string& name) const {
  return svc_->RemoveEntry(name);
}

zx_status_t OutgoingDirectory::AddNamedService(ServiceHandler handler, std::string service,
                                               std::string instance) const {
  auto dir = ::GetOrCreateDirectory(svc_, std::move(service));
  return dir->AddEntry(std::move(instance), handler.TakeDirectory());
}

zx_status_t OutgoingDirectory::RemoveNamedService(const std::string& service,
                                                  const std::string& instance) const {
  vfs::Node* node;
  zx_status_t status = svc_->Lookup(instance, &node);
  if (status != ZX_OK) {
    return ZX_OK;
  }
  return static_cast<vfs::PseudoDir*>(node)->RemoveEntry(service);
}

}  // namespace sys
