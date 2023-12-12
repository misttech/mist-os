// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/bin/vulkan_loader/loader.h"

fidl::ProtocolHandler<fuchsia_vulkan_loader::Loader> LoaderImpl::GetHandler(
    LoaderApp* app, async_dispatcher_t* dispatcher) {
  return [=](fidl::ServerEnd<fuchsia_vulkan_loader::Loader> server_end) {
    std::unique_ptr<LoaderImpl> impl(new LoaderImpl(app));
    auto binding = fidl::BindServer(dispatcher, std::move(server_end), std::move(impl));
  };
}

LoaderImpl::~LoaderImpl() { app_->RemoveObserver(this); }

zx::result<> LoaderImpl::Add(component::OutgoingDirectory& outgoing_dir, LoaderApp* app,
                             async_dispatcher_t* dispatcher) {
  return outgoing_dir.AddUnmanagedProtocol<fuchsia_vulkan_loader::Loader>(
      GetHandler(app, dispatcher));
}

// LoaderApp::Observer implementation.
void LoaderImpl::OnIcdListChanged(LoaderApp* app) {
  auto it = callbacks_.begin();
  while (it != callbacks_.end()) {
    std::optional<zx::vmo> vmo = app->GetMatchingIcd(it->first);
    if (!vmo) {
      ++it;
    } else {
      it->second.Reply(*std::move(vmo));
      it = callbacks_.erase(it);
    }
  }
  if (!app->HavePendingActions()) {
    for (auto& handle : connect_manifest_handles_) {
      app_->ServeManifestFs(std::move(handle));
    }
    connect_manifest_handles_.clear();
  }
  if (!waiting_for_callbacks()) {
    app_->RemoveObserver(this);
  }
}

// fuchsia::vulkan::loader::Loader impl
void LoaderImpl::Get(GetRequest& request, GetCompleter::Sync& completer) {
  AddCallback(std::move(request.name()), completer.ToAsync());
}

void LoaderImpl::ConnectToDeviceFs(ConnectToDeviceFsRequest& request,
                                   ConnectToDeviceFsCompleter::Sync& completer) {
  app_->ServeDeviceFs(fidl::ServerEnd<fuchsia_io::Directory>{std::move(request.channel())});
}

void LoaderImpl::ConnectToManifestFs(ConnectToManifestFsRequest& request,
                                     ConnectToManifestFsCompleter::Sync& completer) {
  auto server_end = fidl::ServerEnd<fuchsia_io::Directory>{std::move(request.channel())};
  if (!(request.options() & fuchsia_vulkan_loader::ConnectToManifestOptions::kWaitForIdle) ||
      !app_->HavePendingActions()) {
    app_->ServeManifestFs(std::move(server_end));
    return;
  }

  bool was_waiting_for_callbacks = waiting_for_callbacks();
  connect_manifest_handles_.push_back(std::move(server_end));
  if (waiting_for_callbacks() && !was_waiting_for_callbacks) {
    app_->AddObserver(this);
  }
}

void LoaderImpl::GetSupportedFeatures(GetSupportedFeaturesCompleter::Sync& completer) {
  constexpr fuchsia_vulkan_loader::Features kFeatures =
      fuchsia_vulkan_loader::Features::kConnectToDeviceFs |
      fuchsia_vulkan_loader::Features::kConnectToManifestFs | fuchsia_vulkan_loader::Features::kGet;
  completer.Reply(kFeatures);
}

void LoaderImpl::AddCallback(std::string name, GetCompleter::Async completer) {
  bool was_waiting_for_callbacks = waiting_for_callbacks();
  std::optional<zx::vmo> vmo = app_->GetMatchingIcd(name);
  if (vmo) {
    completer.Reply(*std::move(vmo));
    return;
  }
  callbacks_.emplace_back(std::move(name), std::move(completer));
  if (waiting_for_callbacks() && !was_waiting_for_callbacks) {
    app_->AddObserver(this);
  }
}
