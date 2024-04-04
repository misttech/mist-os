// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/testing/fake_composite.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fit/result.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

namespace media_audio {
using fuchsia_hardware_audio::Composite;

FakeComposite::FakeComposite(zx::channel server_end, zx::channel client_end,
                             async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher),
      server_end_(std::move(server_end)),
      client_end_(std::move(client_end)) {
  ADR_LOG_METHOD(kLogFakeComposite || kLogObjectLifetimes);

  SetupElementsMap();
}

FakeComposite::~FakeComposite() {
  ADR_LOG_METHOD(kLogFakeComposite || kLogObjectLifetimes);
  signal_processing_binding_.reset();
  binding_.reset();
}

void on_unbind(FakeComposite* fake_composite, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_hardware_audio::Composite> server_end) {
  FX_LOGS(INFO) << "FakeComposite disconnected";
}

fidl::ClientEnd<fuchsia_hardware_audio::Composite> FakeComposite::Enable() {
  ADR_LOG_METHOD(kLogFakeComposite);
  EXPECT_TRUE(server_end_.is_valid());
  EXPECT_TRUE(client_end_.is_valid());
  EXPECT_TRUE(dispatcher_);
  EXPECT_FALSE(binding_);

  binding_ = fidl::BindServer(dispatcher_, std::move(server_end_), this);
  EXPECT_FALSE(server_end_.is_valid());

  return std::move(client_end_);
}

void FakeComposite::DropComposite() {
  ADR_LOG_METHOD(kLogFakeComposite);

  health_completer_.reset();
  watch_topology_completer_.reset();
  for (auto& element_entry_pair : elements_) {
    if (element_entry_pair.second.watch_completer.has_value()) {
      element_entry_pair.second.watch_completer.reset();
    }
  }

  signal_processing_binding_->Close(ZX_ERR_PEER_CLOSED);
  binding_->Close(ZX_ERR_PEER_CLOSED);
}

void FakeComposite::SetupElementsMap() {
  elements_.insert({kSourceDaiElementId, FakeElementRecord{.element = kSourceDaiElement,
                                                           .state = kSourceDaiElementInitState}});
  elements_.insert({kDestDaiElementId, FakeElementRecord{.element = kDestDaiElement,
                                                         .state = kDestDaiElementInitState}});
  elements_.insert({kDestRbElementId, FakeElementRecord{.element = kDestRbElement,
                                                        .state = kDestRbElementInitState}});
  elements_.insert({kSourceRbElementId, FakeElementRecord{.element = kSourceRbElement,
                                                          .state = kSourceRbElementInitState}});

  ASSERT_TRUE(elements_.at(kSourceDaiElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kDestDaiElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kDestRbElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kSourceRbElementId).state_has_changed);

  ASSERT_FALSE(elements_.at(kSourceDaiElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kDestDaiElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kDestRbElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kSourceRbElementId).watch_completer.has_value());
}

void FakeComposite::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  if (responsive_) {
    if (healthy_.has_value()) {
      completer.Reply(fuchsia_hardware_audio::HealthState{{healthy_.value()}});
    } else {
      completer.Reply({});
    }
  } else {
    health_completer_.emplace(completer.ToAsync());  // Just pend it; never respond.
  }
}

void FakeComposite::SignalProcessingConnect(SignalProcessingConnectRequest& request,
                                            SignalProcessingConnectCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);
  if (!supports_signalprocessing_) {
    request.protocol().Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  FX_CHECK(!signal_processing_binding_.has_value())
      << "SignalProcessing already bound (cannot have multiple clients)";
  signal_processing_binding_ = fidl::BindServer(dispatcher_, std::move(request.protocol()), this);
}

void FakeComposite::Reset(ResetCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);

  // Reset any RingBuffers (start, format)
  // Reset any DAIs (start, format)
  // Reset all signalprocessing Elements
  // Reset the signalprocessing Topology

  completer.Reply(fit::ok());
}

void FakeComposite::GetProperties(GetPropertiesCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);

  // Gather the properties and return them.
  fuchsia_hardware_audio::CompositeProperties composite_properties{};
  if (manufacturer_) {
    composite_properties.manufacturer(*manufacturer_);
  }
  if (product_) {
    composite_properties.product(*product_);
  }
  if (uid_) {
    composite_properties.unique_id(*uid_);
  }
  if (clock_domain_) {
    composite_properties.clock_domain(*clock_domain_);
  }

  completer.Reply(composite_properties);
}

void FakeComposite::GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                                         GetRingBufferFormatsCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  if (element_id < kMinRingBufferElementId || element_id > kMaxRingBufferElementId) {
    ADR_WARN_METHOD() << "Element " << element_id << " is out of range";
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
  }

  auto ring_buffer_format_sets = kDefaultRbFormatsMap.find(element_id);
  if (ring_buffer_format_sets == kDefaultRbFormatsMap.end()) {
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  completer.Reply(fit::success(ring_buffer_format_sets->second));
}

void FakeComposite::CreateRingBuffer(CreateRingBufferRequest& request,
                                     CreateRingBufferCompleter::Sync& completer) {
  [[maybe_unused]] auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  completer.Reply(fit::ok());
}

void FakeComposite::GetDaiFormats(GetDaiFormatsRequest& request,
                                  GetDaiFormatsCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  if (element_id < kMinDaiElementId || element_id > kMaxDaiElementId) {
    ADR_WARN_METHOD() << "Element " << element_id << " is out of range";
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
  }

  auto dai_format_sets = kDefaultDaiFormatsMap.find(element_id);
  if (dai_format_sets == kDefaultDaiFormatsMap.end()) {
    ADR_WARN_METHOD() << "No DaiFormatSet found for element " << element_id;
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  completer.Reply(fit::success(dai_format_sets->second));
}

void FakeComposite::SetDaiFormat(SetDaiFormatRequest& request,
                                 SetDaiFormatCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  if (element_id < kMinDaiElementId || element_id > kMaxDaiElementId) {
    ADR_WARN_METHOD() << "Element " << element_id << " is out of range";
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
  }

  completer.Reply(fit::ok());
}

// Return our static element list.
void FakeComposite::GetElements(GetElementsCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);

  completer.Reply(fit::success(kElements));
}

// Return our static topology list.
void FakeComposite::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);

  completer.Reply(fit::success(kTopologies));
}

void FakeComposite::WatchElementState(WatchElementStateRequest& request,
                                      WatchElementStateCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  auto match = elements_.find(element_id);
  if (match == elements_.end()) {
    ADR_WARN_METHOD() << "Element ID " << element_id << " not found";
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  FakeElementRecord& element = match->second;

  if (element.watch_completer.has_value()) {
    ADR_WARN_METHOD() << "previous completer was still pending";
    element.watch_completer.reset();
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  element.watch_completer = completer.ToAsync();

  CheckForElementStateCompletion(element);
}

void FakeComposite::SetElementState(SetElementStateRequest& request,
                                    SetElementStateCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  auto match = elements_.find(element_id);
  if (match == elements_.end()) {
    ADR_WARN_METHOD() << "Element ID " << element_id << " not found";
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  FakeElementRecord& element_record = match->second;

  if (element_record.state == request.state()) {
    ADR_LOG_METHOD(kLogFakeComposite)
        << "element " << element_id << " was already in this state: no change";
  } else {
    element_record.state = request.state();
    element_record.state_has_changed = true;

    CheckForElementStateCompletion(element_record);
  }

  completer.Reply(fit::ok());
}

void FakeComposite::InjectElementStateChange(
    ElementId element_id, fuchsia_hardware_audio_signalprocessing::ElementState new_state) {
  auto match = elements_.find(element_id);
  ASSERT_NE(match, elements_.end());
  auto& element = match->second;

  element.state = std::move(new_state);
  element.state_has_changed = true;

  CheckForElementStateCompletion(element);
}

// static
void FakeComposite::CheckForElementStateCompletion(FakeElementRecord& element_record) {
  if (element_record.state_has_changed && element_record.watch_completer.has_value()) {
    auto completer = std::move(*element_record.watch_completer);
    element_record.watch_completer.reset();

    element_record.state_has_changed = false;

    completer.Reply(element_record.state);
  }
}

void FakeComposite::WatchTopology(WatchTopologyCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);
  if (watch_topology_completer_.has_value()) {
    ADR_WARN_METHOD() << "previous completer was still pending";
    watch_topology_completer_.reset();
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  watch_topology_completer_ = completer.ToAsync();

  CheckForTopologyCompletion();
}

void FakeComposite::SetTopology(SetTopologyRequest& request,
                                SetTopologyCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogFakeComposite);

  bool topology_id_is_valid = false;
  for (const auto& topology : kTopologies) {
    if (topology.id() == request.topology_id()) {
      topology_id_is_valid = true;
      break;
    }
  }
  if (!topology_id_is_valid) {
    ADR_WARN_METHOD() << "Topology ID " << request.topology_id() << " not found";
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (topology_id_.has_value() && *topology_id_ == request.topology_id()) {
    ADR_LOG_METHOD(kLogFakeComposite)
        << "topology was already set to " << request.topology_id() << ": no change";
  } else {
    topology_id_ = request.topology_id();
    topology_has_changed_ = true;

    CheckForTopologyCompletion();
  }
  completer.Reply(fit::ok());
}

// Inject std::nullopt to simulate "no topology", such as at power-up or after Reset().
void FakeComposite::InjectTopologyChange(std::optional<TopologyId> topology_id) {
  if (topology_id.has_value()) {
    topology_id_ = *topology_id;
    topology_has_changed_ = true;

    CheckForTopologyCompletion();
  } else {
    topology_id_.reset();
    topology_has_changed_ = false;  // A new `SetTopology` call must be made
  }
}

void FakeComposite::CheckForTopologyCompletion() {
  if (topology_id_.has_value() && topology_has_changed_ && watch_topology_completer_.has_value()) {
    auto completer = std::move(*watch_topology_completer_);
    watch_topology_completer_.reset();

    topology_has_changed_ = false;

    completer.Reply(*topology_id_);
  }
}

}  // namespace media_audio
