// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "snapshot_annotation_register.h"

static constexpr char kNamespace[] = "sysmem";

SnapshotAnnotationRegister::~SnapshotAnnotationRegister() {
  std::scoped_lock lock(lock_);

  // If a client had been bound with SetServiceDirectory(), it must have already been unbound with a
  // call to UnsetServiceDirectory() on the appropriate thread.
  ZX_ASSERT(!client_.is_valid());
}

void SnapshotAnnotationRegister::AssertRunningOnClientThread() {
  if (client_thread_.has_value()) {
    ZX_ASSERT(std::this_thread::get_id() == *client_thread_);
  } else {
    client_thread_ = std::this_thread::get_id();
  }
}

void SnapshotAnnotationRegister::SetServiceDirectory(
    std::shared_ptr<sys::ServiceDirectory> service_directory, async_dispatcher_t* dispatcher) {
  std::scoped_lock lock(lock_);
  AssertRunningOnClientThread();

  fidl::Client<fuchsia_feedback::ComponentDataRegister> new_client;

  // Opportunistically try to connect, but silently leave new_client with a non-connected channel if
  // unavailable.
  if (service_directory) {
    auto endpoints = fidl::CreateEndpoints<fuchsia_feedback::ComponentDataRegister>();
    ZX_ASSERT_MSG(endpoints.is_ok(), "fidl::CreateEndpoints failed - status: %d",
                  endpoints.error_value());

    new_client.Bind(std::move(endpoints->client), dispatcher);
    service_directory->Connect(
        fidl::DiscoverableProtocolName<fuchsia_feedback::ComponentDataRegister>,
        endpoints->server.TakeChannel());
  }

  client_ = std::move(new_client);
  Flush();  // Send the current state as the initial state.
}

void SnapshotAnnotationRegister::IncrementNumDmaCorruptions() {
  std::scoped_lock lock(lock_);
  AssertRunningOnClientThread();

  num_dma_corruptions_++;
  Flush();
}

void SnapshotAnnotationRegister::Flush() {
  if (client_.is_valid()) {
    std::vector<fuchsia_feedback::Annotation> annotations = {
        {"num-dma-corruptions", std::to_string(num_dma_corruptions_)},
    };

    fuchsia_feedback::ComponentData component_data;
    component_data.namespace_(kNamespace);
    component_data.annotations(std::move(annotations));

    client_->Upsert(std::move(component_data)).Then([](auto) { /* ignore result */ });
  }
}
