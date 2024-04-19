// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/testing/fake_composite.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/markers.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fit/result.h>
#include <zircon/errors.h>

#include <string>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/testing/fake_composite_ring_buffer.h"

namespace media_audio {
using fuchsia_hardware_audio::Composite;

bool FormatIsSupported(
    const fuchsia_hardware_audio::Format& format,
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& ring_buffer_format_sets) {
  if (!format.pcm_format().has_value()) {
    return false;
  }

  for (const auto& format_set : ring_buffer_format_sets) {
    bool match = false;
    for (const auto& frame_rate : *format_set.pcm_supported_formats()->frame_rates()) {
      if (frame_rate == format.pcm_format()->frame_rate()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& sample_format : *format_set.pcm_supported_formats()->sample_formats()) {
      if (sample_format == format.pcm_format()->sample_format()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& channel_set : *format_set.pcm_supported_formats()->channel_sets()) {
      if (channel_set.attributes()->size() == format.pcm_format()->number_of_channels()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& bytes_per_sample : *format_set.pcm_supported_formats()->bytes_per_sample()) {
      if (bytes_per_sample == format.pcm_format()->bytes_per_sample()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& valid_bits : *format_set.pcm_supported_formats()->valid_bits_per_sample()) {
      if (valid_bits == format.pcm_format()->valid_bits_per_sample()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }
    return true;
  }

  return false;
}

FakeComposite::FakeComposite(zx::channel server_end, zx::channel client_end,
                             async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher),
      server_end_(std::move(server_end)),
      client_end_(std::move(client_end)) {
  ADR_LOG_METHOD(kLogFakeComposite || kLogObjectLifetimes);

  SetupElementsMap();
}

// From the device side, drop the Composite protocol connection as if the device has been removed.
void FakeComposite::DropComposite() {
  FX_CHECK(binding_.has_value()) << "Should not call DropComposite() twice";

  binding_->Close(ZX_ERR_PEER_CLOSED);  // This in turn will trigger on_unbind -> DropChildren().
  binding_.reset();
}

void on_unbind(FakeComposite* fake_composite, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_hardware_audio::Composite> server_end) {
  ADR_LOG(kLogFakeComposite) << "for FakeComposite";
  fake_composite->DropChildren();
}

void FakeComposite::DropChildren() {
  ADR_LOG_METHOD(kLogFakeComposite);

  health_completer_.reset();
  watch_topology_completer_.reset();

  DropRingBuffers();

  for (auto& element_entry_pair : elements_) {
    if (element_entry_pair.second.watch_completer.has_value()) {
      element_entry_pair.second.watch_completer.reset();
    }
  }
  if (signal_processing_binding_.has_value()) {
    signal_processing_binding_->Close(ZX_ERR_PEER_CLOSED);
    signal_processing_binding_.reset();
  }
}

// From the driver side, drop the RingBuffer protocol connection for this element_id.
void FakeComposite::DropRingBuffers() {
  ADR_LOG_METHOD(kLogFakeComposite);

  for (auto& binding : ring_buffer_bindings_) {
    binding.second.Unbind();
  }
}

// From the driver side, drop the RingBuffer protocol connection for this element_id.
void FakeComposite::DropRingBuffer(ElementId element_id) {
  ADR_LOG_METHOD(kLogFakeComposite) << "element_id " << element_id;

  for (auto& binding : ring_buffer_bindings_) {
    if (binding.first == element_id) {
      binding.second.Unbind();
      return;
    }
  }
  ADR_WARN_METHOD() << "No ring_buffer binding found for element_id " << element_id;
}

// static
void FakeComposite::on_rb_unbind(FakeCompositeRingBuffer* fake_ring_buffer, fidl::UnbindInfo info,
                                 fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer>) {
  ADR_LOG(kLogFakeComposite) << "for FakeCompositeRingBuffer";

  fake_ring_buffer->parent()->RingBufferWasDropped(fake_ring_buffer->element_id());
}

// The RingBuffer FIDL connection has already been dropped, so there's nothing else for the parent
// driver to do, except clean up our accounting.
void FakeComposite::RingBufferWasDropped(ElementId element_id) {
  ADR_LOG_METHOD(kLogFakeComposite) << "element_id " << element_id;

  ring_buffer_bindings_.erase((element_id));
  ring_buffers_.erase(element_id);
}

fidl::ClientEnd<fuchsia_hardware_audio::Composite> FakeComposite::Enable() {
  ADR_LOG_METHOD(kLogFakeComposite);
  EXPECT_TRUE(server_end_.is_valid());
  EXPECT_TRUE(client_end_.is_valid());
  EXPECT_TRUE(dispatcher_);
  EXPECT_FALSE(binding_);

  binding_ = fidl::BindServer(dispatcher_, std::move(server_end_), shared_from_this(), &on_unbind);
  EXPECT_FALSE(server_end_.is_valid());

  return std::move(client_end_);
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
  elements_.insert(
      {kMuteElementId, FakeElementRecord{.element = kMuteElement, .state = kMuteElementInitState}});

  ASSERT_TRUE(elements_.at(kSourceDaiElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kDestDaiElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kDestRbElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kSourceRbElementId).state_has_changed);
  ASSERT_TRUE(elements_.at(kMuteElementId).state_has_changed);

  ASSERT_FALSE(elements_.at(kSourceDaiElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kDestDaiElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kDestRbElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kSourceRbElementId).watch_completer.has_value());
  ASSERT_FALSE(elements_.at(kMuteElementId).watch_completer.has_value());
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
  DropRingBuffers();

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

  auto element_pair_iter = elements_.find(element_id);
  if (element_pair_iter == elements_.end()) {
    ADR_WARN_METHOD() << "unrecognized element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  if (*element_pair_iter->second.element.type() !=
          fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint ||
      *element_pair_iter->second.element.type_specific()->endpoint()->type() !=
          fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer) {
    ADR_WARN_METHOD() << "wrong type for element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kWrongType));
    return;
  }

  auto ring_buffer_format_sets = kDefaultRbFormatsMap.find(element_id);
  if (ring_buffer_format_sets == kDefaultRbFormatsMap.end()) {
    ADR_WARN_METHOD() << "no ring_buffer_format_sets specified for element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  completer.Reply(fit::success(ring_buffer_format_sets->second));
}

void FakeComposite::ReserveRingBufferSize(ElementId element_id, size_t size) {
  ring_buffer_allocation_sizes_.insert_or_assign(element_id, size);
}

void FakeComposite::EnableActiveChannelsSupport(ElementId element_id) {
  active_channels_support_overrides_.insert_or_assign(element_id, true);
}
void FakeComposite::DisableActiveChannelsSupport(ElementId element_id) {
  active_channels_support_overrides_.insert_or_assign(element_id, false);
}
void FakeComposite::PresetTurnOnDelay(ElementId element_id,
                                      std::optional<zx::duration> turn_on_delay) {
  turn_on_delay_overrides_.insert_or_assign(element_id, turn_on_delay);
}
void FakeComposite::PresetInternalExternalDelays(ElementId element_id, zx::duration internal_delay,
                                                 std::optional<zx::duration> external_delay) {
  internal_delay_overrides_.insert_or_assign(element_id, internal_delay);
  external_delay_overrides_.insert_or_assign(element_id, external_delay);
}

void FakeComposite::CreateRingBuffer(CreateRingBufferRequest& request,
                                     CreateRingBufferCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  auto element_pair_iter = elements_.find(element_id);
  if (element_pair_iter == elements_.end()) {
    ADR_WARN_METHOD() << "unrecognized element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  if (*element_pair_iter->second.element.type() !=
          fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint ||
      *element_pair_iter->second.element.type_specific()->endpoint()->type() !=
          fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer) {
    ADR_WARN_METHOD() << "wrong type for element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kWrongType));
    return;
  }
  auto ring_buffer_format_sets_iter = kDefaultRbFormatsMap.find(element_id);
  if (ring_buffer_format_sets_iter == kDefaultRbFormatsMap.end()) {
    ADR_WARN_METHOD() << "no ring_buffer_format_sets specified for element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  const auto& ring_buffer_format_sets = ring_buffer_format_sets_iter->second;
  // Make sure the Format is OK
  if (!FormatIsSupported(request.format(), ring_buffer_format_sets)) {
    ADR_WARN_METHOD() << "ring_buffer_format not supported for element_id " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kNotSupported));
    return;
  }

  // Make sure the server_end is OK
  if (!request.ring_buffer().is_valid()) {
    ADR_WARN_METHOD() << "ring_buffer server_end is invalid";
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  size_t ring_buffer_allocated_size = kDefaultRingBufferAllocationSize;
  auto match = ring_buffer_allocation_sizes_.find(element_id);
  if (match != ring_buffer_allocation_sizes_.end()) {
    ring_buffer_allocated_size = match->second;
  } else {
    ADR_WARN_METHOD() << "ring buffer allocation size not found";
  }

  auto ring_buffer_impl = std::make_unique<FakeCompositeRingBuffer>(
      this, element_id, *request.format().pcm_format(), ring_buffer_allocated_size);

  auto match_active_channels = active_channels_support_overrides_.find(element_id);
  if (match_active_channels != active_channels_support_overrides_.end()) {
    if (match_active_channels->second) {
      ring_buffer_impl->enable_active_channels_support();
    } else {
      ring_buffer_impl->disable_active_channels_support();
    }
  }

  auto match_turn_on_delay = turn_on_delay_overrides_.find(element_id);
  if (match_turn_on_delay != turn_on_delay_overrides_.end()) {
    match_turn_on_delay->second ? ring_buffer_impl->set_turn_on_delay(*match_turn_on_delay->second)
                                : ring_buffer_impl->clear_turn_on_delay();
  }
  auto match_internal_delay = internal_delay_overrides_.find(element_id);
  if (match_internal_delay != internal_delay_overrides_.end()) {
    ring_buffer_impl->set_internal_delay(match_internal_delay->second);
  }
  auto match_external_delay = external_delay_overrides_.find(element_id);
  if (match_external_delay != external_delay_overrides_.end()) {
    match_external_delay->second
        ? ring_buffer_impl->set_external_delay(*match_external_delay->second)
        : ring_buffer_impl->clear_external_delay();
  }

  ring_buffers_.erase(element_id);
  ring_buffers_.insert({element_id, std::move(ring_buffer_impl)});
  ring_buffer_bindings_.insert_or_assign(
      element_id,
      fidl::BindServer(dispatcher_, std::move(request.ring_buffer()),
                       ring_buffers_.at(element_id).get(), &FakeComposite::on_rb_unbind));

  completer.Reply(fit::ok());
}

void FakeComposite::GetDaiFormats(GetDaiFormatsRequest& request,
                                  GetDaiFormatsCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  if (element_id < kMinDaiElementId || element_id > kMaxDaiElementId) {
    ADR_WARN_METHOD() << "Element " << element_id << " is out of range";
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  auto dai_format_sets = kDefaultDaiFormatsMap.find(element_id);
  if (dai_format_sets == kDefaultDaiFormatsMap.end()) {
    ADR_WARN_METHOD() << "No DaiFormatSet found for element " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  completer.Reply(fit::success(dai_format_sets->second));
}

// static
bool FakeComposite::DaiFormatIsSupported(ElementId element_id,
                                         const fuchsia_hardware_audio::DaiFormat& format) {
  auto match = kDefaultDaiFormatsMap.find(element_id);
  if (match == kDefaultDaiFormatsMap.end()) {
    return false;
  }

  if (format.channels_to_use_bitmask() >= (1u << format.number_of_channels())) {
    return false;
  }

  auto dai_format_sets = match->second;
  for (const auto& dai_format_set : dai_format_sets) {
    bool match = false;
    for (auto channel_count : dai_format_set.number_of_channels()) {
      if (channel_count == format.number_of_channels()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (auto sample_format : dai_format_set.sample_formats()) {
      if (sample_format == format.sample_format()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& frame_format : dai_format_set.frame_formats()) {
      if (frame_format == format.frame_format()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (auto rate : dai_format_set.frame_rates()) {
      if (rate == format.frame_rate()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (auto bits : dai_format_set.bits_per_slot()) {
      if (bits == format.bits_per_slot()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (auto bits : dai_format_set.bits_per_sample()) {
      if (bits == format.bits_per_sample()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }
    // This DaiFormatSet survived with a match on all aspects.
    return true;
  }
  // None of the DaiFormatSets survived through all of the aspects.]
  return false;
}

void FakeComposite::SetDaiFormat(SetDaiFormatRequest& request,
                                 SetDaiFormatCompleter::Sync& completer) {
  auto element_id = request.processing_element_id();
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";

  if (element_id < kMinDaiElementId || element_id > kMaxDaiElementId) {
    ADR_WARN_METHOD() << "Element " << element_id << " is out of range";
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  if (!DaiFormatIsSupported(element_id, request.format())) {
    ADR_WARN_METHOD() << "Format is not supported for element " << element_id;
    completer.Reply(fit::error(fuchsia_hardware_audio::DriverError::kNotSupported));
    return;
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

  MaybeCompleteWatchElementState(element);
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

    MaybeCompleteWatchElementState(element_record);
  }

  completer.Reply(fit::ok());
}

void FakeComposite::InjectElementStateChange(
    ElementId element_id, fuchsia_hardware_audio_signalprocessing::ElementState new_state) {
  ADR_LOG_METHOD(kLogFakeComposite) << "(" << element_id << ")";
  auto match = elements_.find(element_id);
  ASSERT_NE(match, elements_.end());
  auto& element = match->second;

  element.state = std::move(new_state);
  element.state_has_changed = true;

  MaybeCompleteWatchElementState(element);
}

// static
void FakeComposite::MaybeCompleteWatchElementState(FakeElementRecord& element_record) {
  if (element_record.state_has_changed && element_record.watch_completer.has_value()) {
    auto completer = std::move(*element_record.watch_completer);
    element_record.watch_completer.reset();

    element_record.state_has_changed = false;

    ADR_LOG_STATIC(kLogFakeComposite)
        << "About to complete WatchElementState for element_id " << *element_record.element.id();
    completer.Reply(element_record.state);
  } else {
    ADR_LOG_STATIC(kLogFakeComposite)
        << "Not completing WatchElementState for element_id " << *element_record.element.id();
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

  MaybeCompleteWatchTopology();
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

    MaybeCompleteWatchTopology();
  }
  completer.Reply(fit::ok());
}

// Inject std::nullopt to simulate "no topology", such as at power-up or after Reset().
void FakeComposite::InjectTopologyChange(std::optional<TopologyId> topology_id) {
  topology_has_changed_ = topology_id.has_value();

  if (topology_has_changed_) {
    topology_id_ = *topology_id;

    MaybeCompleteWatchTopology();
  } else {
    topology_id_.reset();  // A new `SetTopology` call must be made
  }
}

void FakeComposite::MaybeCompleteWatchTopology() {
  if (topology_id_.has_value() && topology_has_changed_ && watch_topology_completer_.has_value()) {
    auto completer = std::move(*watch_topology_completer_);
    watch_topology_completer_.reset();

    topology_has_changed_ = false;

    ADR_LOG_STATIC(kLogFakeComposite)
        << "About to complete WatchTopology with topology_id " << *topology_id_;
    completer.Reply(*topology_id_);
  } else {
    ADR_LOG_STATIC(kLogFakeComposite) << "Not completing WatchTopology";
  }
}

}  // namespace media_audio
