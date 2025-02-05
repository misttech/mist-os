// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/inspector.h"

#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/vmo/types.h>

#include <memory>
#include <string>

#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

// static
// This singleton handles Inspect for the entire service.
std::unique_ptr<Inspector> Inspector::singleton_ = nullptr;

// static
void Inspector::Initialize(async_dispatcher_t* dispatcher) {
  ADR_LOG_STATIC(kTraceInspector);
  // Should only be called once.
  if (singleton_ == nullptr) {
    singleton_ = std::make_unique<Inspector>(dispatcher);
  } else {
    FX_LOGS(ERROR) << "Inspector::Initialize should only be called once";
  }
}

///////////////////////////////////////
// SetActiveChannelsInspectInstance methods
SetActiveChannelsInspectInstance::SetActiveChannelsInspectInstance(
    inspect::Node set_active_channels_node, uint64_t channel_bitmask, const zx::time& called_at,
    const zx::time& completed_at)
    : set_active_channels_node_(std::move(set_active_channels_node)) {
  ADR_LOG_METHOD(kTraceInspector);
  called_at_ = set_active_channels_node_.CreateInt(kCalledAt, called_at.get());
  completed_at_ = set_active_channels_node_.CreateInt(kCompletedAt, completed_at.get());
  channel_bitmask_ = set_active_channels_node_.CreateUint(kChannelBitmask, channel_bitmask);
}

SetActiveChannelsInspectInstance::~SetActiveChannelsInspectInstance() {
  ADR_LOG_METHOD(kTraceInspector);
}

///////////////////////////////////////
// RunningIntervalInspectInstance methods
RunningIntervalInspectInstance::RunningIntervalInspectInstance(inspect::Node running_interval_node,
                                                               const zx::time& started_at)
    : running_interval_node_(std::move(running_interval_node)) {
  ADR_LOG_METHOD(kTraceInspector);
  started_at_ = running_interval_node_.CreateInt(kStartedAt, started_at.get());
}

RunningIntervalInspectInstance::~RunningIntervalInspectInstance() {
  ADR_LOG_METHOD(kTraceInspector);
}

void RunningIntervalInspectInstance::RecordStopTime(const zx::time& stopped_at) {
  ADR_LOG_METHOD(kTraceInspector) << kStoppedAt << stopped_at.get();
  stopped_at_ = running_interval_node_.CreateInt(kStoppedAt, stopped_at.get());
}

///////////////////////////////////////
// RingBufferInspectInstance methods
RingBufferInspectInstance::RingBufferInspectInstance(inspect::Node ring_buffer_instance_node,
                                                     const zx::time& created_at)
    : ring_buffer_instance_node_(std::move(ring_buffer_instance_node)) {
  ADR_LOG_METHOD(kTraceInspector);
  created_at_ = ring_buffer_instance_node_.CreateInt(kCreatedAt, created_at.get());
}

RingBufferInspectInstance::~RingBufferInspectInstance() { ADR_LOG_METHOD(kTraceInspector); }

void RingBufferInspectInstance::RecordDestructionTime(const zx::time& destroyed_at) {
  ADR_LOG_METHOD(kTraceInspector) << kDestroyedAt << destroyed_at.get();
  destroyed_at_ = ring_buffer_instance_node_.CreateInt(kDestroyedAt, destroyed_at.get());
}

void RingBufferInspectInstance::RecordStartTime(const zx::time& started_at) {
  ADR_LOG_METHOD(kTraceInspector);
  if (running_intervals_.empty()) {
    running_intervals_root_node_ = ring_buffer_instance_node_.CreateChild(kRunningIntervals);
  }
  auto running_interval_node =
      running_intervals_root_node_.CreateChild(std::to_string(running_intervals_.size()));
  auto running_interval = std::make_shared<RunningIntervalInspectInstance>(
      std::move(running_interval_node), started_at);
  running_intervals_.push_back(running_interval);
}

void RingBufferInspectInstance::RecordStopTime(const zx::time& stopped_at) {
  ADR_LOG_METHOD(kTraceInspector) << kStoppedAt << stopped_at.get();
  if (!running_intervals_.empty()) {
    (*running_intervals_.rbegin())->RecordStopTime(stopped_at);
  }
}

std::shared_ptr<SetActiveChannelsInspectInstance>
RingBufferInspectInstance::RecordSetActiveChannelsCall(uint64_t channel_bitmask,
                                                       const zx::time& called_at,
                                                       const zx::time& completed_at) {
  ADR_LOG_METHOD(kTraceInspector);
  if (set_active_channels_calls_.empty()) {
    set_active_channels_calls_root_node_ =
        ring_buffer_instance_node_.CreateChild(kSetActiveChannelsCalls);
  }
  auto set_active_channels_instance_node = set_active_channels_calls_root_node_.CreateChild(
      std::to_string(set_active_channels_calls_.size()));
  auto set_active_channels_instance = std::make_shared<SetActiveChannelsInspectInstance>(
      std::move(set_active_channels_instance_node), channel_bitmask, called_at, completed_at);

  set_active_channels_calls_.push_back(set_active_channels_instance);
  return set_active_channels_instance;
}

///////////////////////////////////////
// RingBufferElement methods
RingBufferElement::RingBufferElement(inspect::Node ring_buffer_element_node, ElementId element_id)
    : ring_buffer_element_node_(std::move(ring_buffer_element_node)), element_id_(element_id) {
  ADR_LOG_METHOD(kTraceInspector);
  element_id_property_ = ring_buffer_element_node_.CreateUint(kElementId, element_id);
}

RingBufferElement::~RingBufferElement() { ADR_LOG_METHOD(kTraceInspector); }

std::shared_ptr<RingBufferInspectInstance> RingBufferElement::RecordRingBufferInstance(
    const zx::time& created_at) {
  auto ring_buffer_instance_node = ring_buffer_element_node_.CreateChild(
      std::string("instance ") + std::to_string(ring_buffer_instances_.size()));
  auto ring_buffer_instance =
      std::make_shared<RingBufferInspectInstance>(std::move(ring_buffer_instance_node), created_at);
  ADR_LOG_METHOD(kTraceInspector) << "returning " << ring_buffer_instance;

  ring_buffer_instances_.push_back(ring_buffer_instance);
  return ring_buffer_instance;
}

///////////////////////////////////////
// DeviceInspectInstance methods
DeviceInspectInstance::DeviceInspectInstance(inspect::Node device_node, std::string device_name,
                                             fuchsia_audio_device::DeviceType device_type,
                                             const zx::time& added_at)
    : device_node_(std::move(device_node)), name_(std::move(device_name)) {
  ADR_LOG_METHOD(kTraceInspector);
  std::stringstream device_type_ss;
  added_at_ = device_node_.CreateInt(kAddedAt, added_at.get());

  device_type_ss << device_type;
  type_ = device_node_.CreateString(kDeviceType, device_type_ss.str());
}

DeviceInspectInstance::~DeviceInspectInstance() { ADR_LOG_METHOD(kTraceInspector); }

void DeviceInspectInstance::RecordTokenId(TokenId token_id) {
  ADR_LOG_METHOD(kTraceInspector);
  token_id_ = device_node_.CreateUint(kTokenId, token_id);
}

void DeviceInspectInstance::RecordDeviceHealthOk() {
  ADR_LOG_METHOD(kTraceInspector);
  healthy_ = device_node_.CreateBool(kHealthy, true);
}

void DeviceInspectInstance::RecordProperties(std::optional<bool> is_input,
                                             std::optional<std::string> manufacturer,
                                             std::optional<std::string> product,
                                             std::optional<std::string> unique_instance_id,
                                             std::optional<ClockDomain> clock_domain) {
  ADR_LOG_METHOD(kTraceInspector);
  if (is_input.has_value()) {
    is_input_ = device_node_.CreateBool(kIsInput, *is_input);
  }
  if (manufacturer.has_value()) {
    manufacturer_ = device_node_.CreateString(kManufacturer, *manufacturer);
  }
  if (product.has_value()) {
    product_ = device_node_.CreateString(kProduct, *product);
  }
  if (unique_instance_id.has_value()) {
    unique_instance_id_ = device_node_.CreateString(kUniqueId, *unique_instance_id);
  }
  if (clock_domain.has_value()) {
    clock_domain_ = device_node_.CreateUint(kClockDomain, *clock_domain);
  }
}

std::shared_ptr<RingBufferElement> DeviceInspectInstance::RecordRingBufferElement(
    ElementId element_id) {
  ADR_LOG_METHOD(kTraceInspector);
  if (ring_buffer_elements_.empty()) {
    ring_buffers_root_node_ = device_node_.CreateChild(kRingBufferElements);
  }
  auto ring_buffer_element_node =
      ring_buffers_root_node_.CreateChild(std::to_string(ring_buffer_elements_.size()));
  auto ring_buffer_element =
      std::make_shared<RingBufferElement>(std::move(ring_buffer_element_node), element_id);

  ring_buffer_elements_.push_back(ring_buffer_element);
  return ring_buffer_element;
}

std::shared_ptr<RingBufferInspectInstance> DeviceInspectInstance::RecordRingBufferInstance(
    ElementId element_id, const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector) << kElementId << element_id;
  auto found = std::find_if(ring_buffer_elements_.begin(), ring_buffer_elements_.end(),
                            [element_id](std::shared_ptr<RingBufferElement> rb_element) {
                              return (rb_element->element_id() == element_id);
                            });
  if (found == ring_buffer_elements_.end()) {
    ADR_WARN_OBJECT() << "Cannot create RB inspect instance: element_id " << element_id
                      << " not found";
    return nullptr;
  }
  return (*found)->RecordRingBufferInstance(created_at);
}

void DeviceInspectInstance::RecordError(const zx::time& failed_at) {
  ADR_LOG_METHOD(kTraceInspector);
  healthy_ = device_node_.CreateBool(kHealthy, false);
  failed_at_ = device_node_.CreateInt(kFailedAt, failed_at.get());
}

void DeviceInspectInstance::RecordRemoval(const zx::time& removed_at) {
  ADR_LOG_METHOD(kTraceInspector);
  removed_at_ = device_node_.CreateInt(kRemovedAt, removed_at.get());
}

///////////////////////////////////////
// FidlServerInspectInstance methods
FidlServerInspectInstance::FidlServerInspectInstance(inspect::Node instance_node,
                                                     const zx::time& created_at)
    : instance_node_(std::move(instance_node)) {
  ADR_LOG_METHOD(kTraceInspector);
  created_at_ = instance_node_.CreateInt(kCreatedAt, created_at.get());
}

FidlServerInspectInstance::~FidlServerInspectInstance() { ADR_LOG_METHOD(kTraceInspector); }

void FidlServerInspectInstance::RecordDestructionTime(const zx::time& destroyed_at) {
  ADR_LOG_METHOD(kTraceInspector);
  destroyed_at_ = instance_node_.CreateInt(kDestroyedAt, destroyed_at.get());
}

///////////////////////////////////////
// ProviderInspectInstance methods
ProviderInspectInstance::ProviderInspectInstance(inspect::Node provider_instance_node,
                                                 const zx::time& created_at)
    : FidlServerInspectInstance(std::move(provider_instance_node), created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  provider_devices_root_node_ = instance_node().CreateChild(kAddedDevices);
}

ProviderInspectInstance::~ProviderInspectInstance() { ADR_LOG_METHOD(kTraceInspector); }

void ProviderInspectInstance::RecordAddedDevice(const std::string& device_name,
                                                const fuchsia_audio_device::DeviceType& device_type,
                                                const zx::time& added_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node = provider_devices_root_node_.CreateChild(device_name);
  auto instance = std::make_shared<DeviceInspectInstance>(std::move(instance_node), device_name,
                                                          device_type, added_at);
  provider_devices_.push_back(instance);
}

///////////////////////////////////////
// Inspector methods
Inspector::Inspector(async_dispatcher_t* dispatcher)
    : component_inspector_(
          std::make_unique<inspect::ComponentInspector>(dispatcher, inspect::PublishOptions{})),
      inspect_root_(component_inspector_->root()) {
  ADR_LOG_METHOD(kTraceInspector);
  component_inspector_->Health().StartingUp();

  devices_root_ = inspect_root_.CreateChild(kDevices);
  fidl_servers_root_ = inspect_root_.CreateChild(kFidlServers);

  registry_servers_root_ = fidl_servers_root_.CreateChild(kRegistryServerInstances);
  observer_servers_root_ = fidl_servers_root_.CreateChild(kObserverServerInstances);
  control_creator_servers_root_ = fidl_servers_root_.CreateChild(kControlCreatorServerInstances);
  control_servers_root_ = fidl_servers_root_.CreateChild(kControlServerInstances);
  ring_buffer_servers_root_ = fidl_servers_root_.CreateChild(kRingBufferServerInstances);
  provider_servers_root_ = fidl_servers_root_.CreateChild(kProviderServerInstances);

  count_devices_failed_to_connect_ = inspect_root_.CreateUint(kDetectionConnectionErrors, 0);
  count_device_watcher_failures_ = inspect_root_.CreateUint(kDetectionOtherErrors, 0);
  count_detected_unsupported_device_type_ =
      inspect_root_.CreateUint(kDetectionUnsupportedDevices, 0);
}

Inspector::~Inspector() { ADR_LOG_METHOD(kTraceInspector); }

void Inspector::RecordHealthOk() {
  ADR_LOG_METHOD(kTraceInspector);
  component_inspector_->Health().Ok();
}

void Inspector::RecordUnhealthy(const std::string& health_message) {
  ADR_LOG_METHOD(kTraceInspector);
  component_inspector_->Health().Unhealthy(health_message);
}

void Inspector::RecordDetectionFailureToConnect() {
  ADR_LOG_METHOD(kTraceInspector);
  count_devices_failed_to_connect_.Add(1);
}

void Inspector::RecordDetectionFailureOther() {
  ADR_LOG_METHOD(kTraceInspector);
  count_device_watcher_failures_.Add(1);
}

void Inspector::RecordDetectionFailureUnsupported() {
  ADR_LOG_METHOD(kTraceInspector);
  count_detected_unsupported_device_type_.Add(1);
}

std::shared_ptr<DeviceInspectInstance> Inspector::RecordDeviceInitializing(
    const std::string& device_name, fuchsia_audio_device::DeviceType device_type,
    const zx::time& added_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node = devices_root_.CreateChild(device_name);
  auto instance = std::make_shared<DeviceInspectInstance>(std::move(instance_node), device_name,
                                                          device_type, added_at);
  device_instances_.push_back(instance);
  return instance;
}

std::shared_ptr<FidlServerInspectInstance> Inspector::RecordRegistryInstance(
    const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node =
      registry_servers_root_.CreateChild(std::to_string(registry_server_instances_.size()));
  auto fidl_instance =
      std::make_shared<FidlServerInspectInstance>(std::move(instance_node), created_at);
  registry_server_instances_.push_back(fidl_instance);
  return fidl_instance;
}

std::shared_ptr<FidlServerInspectInstance> Inspector::RecordObserverInstance(
    const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node =
      observer_servers_root_.CreateChild(std::to_string(observer_server_instances_.size()));
  auto fidl_instance =
      std::make_shared<FidlServerInspectInstance>(std::move(instance_node), created_at);
  observer_server_instances_.push_back(fidl_instance);
  return fidl_instance;
}

std::shared_ptr<FidlServerInspectInstance> Inspector::RecordControlCreatorInstance(
    const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node = control_creator_servers_root_.CreateChild(
      std::to_string(control_creator_server_instances_.size()));
  auto fidl_instance =
      std::make_shared<FidlServerInspectInstance>(std::move(instance_node), created_at);
  control_creator_server_instances_.push_back(fidl_instance);
  return fidl_instance;
}

std::shared_ptr<FidlServerInspectInstance> Inspector::RecordControlInstance(
    const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node =
      control_servers_root_.CreateChild(std::to_string(control_server_instances_.size()));
  auto fidl_instance =
      std::make_shared<FidlServerInspectInstance>(std::move(instance_node), created_at);
  control_server_instances_.push_back(fidl_instance);
  return fidl_instance;
}

std::shared_ptr<FidlServerInspectInstance> Inspector::RecordRingBufferInstance(
    const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node =
      ring_buffer_servers_root_.CreateChild(std::to_string(ring_buffer_server_instances_.size()));
  auto fidl_instance =
      std::make_shared<FidlServerInspectInstance>(std::move(instance_node), created_at);
  ring_buffer_server_instances_.push_back(fidl_instance);
  return fidl_instance;
}

std::shared_ptr<ProviderInspectInstance> Inspector::RecordProviderInspectInstance(
    const zx::time& created_at) {
  ADR_LOG_METHOD(kTraceInspector);
  auto instance_node =
      provider_servers_root_.CreateChild(std::to_string(provider_server_instances_.size()));
  auto fidl_instance =
      std::make_shared<ProviderInspectInstance>(std::move(instance_node), created_at);
  provider_server_instances_.push_back(fidl_instance);
  return fidl_instance;
}

}  // namespace media_audio
