// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_INSPECTOR_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_INSPECTOR_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <lib/async/dispatcher.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/vmo/types.h>

#include <string>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

const inspect::StringReference kDetectionConnectionErrors(
    "Device detection connection failures (count)");
const inspect::StringReference kDetectionOtherErrors(
    "Device detection unclassified errors (count)");
const inspect::StringReference kDetectionUnsupportedDevices(
    "Device detection unsupported devices (count)");

const inspect::StringReference kDevices("Devices");
const inspect::StringReference kAddedAt("added at");
const inspect::StringReference kFailedAt("failed at");
const inspect::StringReference kRemovedAt("removed at");

const inspect::StringReference kDeviceType("type");
const inspect::StringReference kTokenId("token id");
const inspect::StringReference kHealthy("healthy");
const inspect::StringReference kIsInput("is_input");
const inspect::StringReference kManufacturer("manufacturer");
const inspect::StringReference kProduct("product");
const inspect::StringReference kUniqueId("unique id");
const inspect::StringReference kClockDomain("clock domain");

const inspect::StringReference kRingBufferElements("ring buffer elements");
const inspect::StringReference kElementId("element id");
const inspect::StringReference kRunningIntervals("Running intervals");
const inspect::StringReference kStartedAt("started at");
const inspect::StringReference kStoppedAt("stopped at");
const inspect::StringReference kSetActiveChannelsCalls("SetActiveChannels calls");
const inspect::StringReference kCalledAt("called at");
const inspect::StringReference kCompletedAt("completed at");
const inspect::StringReference kChannelBitmask("channel bitmask");

const inspect::StringReference kFidlServers("FIDL servers");
const inspect::StringReference kRegistryServerInstances("RegistryServer instances");
const inspect::StringReference kObserverServerInstances("ObserverServer instances");
const inspect::StringReference kControlCreatorServerInstances("ControlCreatorServer instances");
const inspect::StringReference kControlServerInstances("ControlServer instances");
const inspect::StringReference kRingBufferServerInstances("RingBufferServer instances");
const inspect::StringReference kProviderServerInstances("ProviderServer instances");
const inspect::StringReference kCreatedAt("created at");
const inspect::StringReference kDestroyedAt("destroyed at");
const inspect::StringReference kAddedDevices("Added devices");

//
// These classes encapsulate the creation and update of the fuchsia Inspect data store. The parent
// Inspector singleton entails the system-provided ComponentInspector and offers methods that enable
// ADR to chronicle objects without integrating directly with Inspect.
//
// Each class handles Inspect for an associated entity (which may not be an actual ADR object).

// This represents a single call to SetActiveChannels.
class SetActiveChannelsInspectInstance {
 public:
  SetActiveChannelsInspectInstance(inspect::Node set_active_channels_node, uint64_t channel_bitmask,
                                   const zx::time& called_at, const zx::time& completed_at);
  ~SetActiveChannelsInspectInstance();

 private:
  static inline const std::string_view kClassName = "SetActiveChannelsInspectInstance";

  inspect::Node set_active_channels_node_;
  inspect::IntProperty called_at_;
  inspect::UintProperty channel_bitmask_;
  inspect::IntProperty completed_at_;
};

// This represents a single Start/Stop segment.
class RunningIntervalInspectInstance {
 public:
  RunningIntervalInspectInstance(inspect::Node running_interval_node, const zx::time& started_at);
  ~RunningIntervalInspectInstance();

  void RecordStopTime(const zx::time& stopped_at);

 private:
  static inline const std::string_view kClassName = "RunningIntervalInspectInstance";

  inspect::Node running_interval_node_;
  inspect::IntProperty started_at_;
  inspect::IntProperty stopped_at_;
};

// This represents an active instance of the audio driver RingBuffer protocol.
class RingBufferInspectInstance {
 public:
  RingBufferInspectInstance(inspect::Node ring_buffer_instance_node, const zx::time& created_at);
  ~RingBufferInspectInstance();

  void RecordDestructionTime(const zx::time& destroyed_at);

  void RecordStartTime(const zx::time& started_at);
  void RecordStopTime(const zx::time& stopped_at);

  inspect::Node& inspect_node() { return ring_buffer_instance_node_; }
  std::shared_ptr<SetActiveChannelsInspectInstance> RecordSetActiveChannelsCall(
      uint64_t channel_bitmask, const zx::time& called_at, const zx::time& completed_at);

 private:
  static inline const std::string_view kClassName = "RingBufferInspectInstance";

  inspect::Node ring_buffer_instance_node_;
  inspect::IntProperty created_at_;
  inspect::IntProperty destroyed_at_;

  inspect::Node set_active_channels_calls_root_node_;
  std::vector<std::shared_ptr<SetActiveChannelsInspectInstance>> set_active_channels_calls_;

  inspect::Node running_intervals_root_node_;
  std::vector<std::shared_ptr<RunningIntervalInspectInstance>> running_intervals_;
};

// This represents a ring buffer element expressed in the hardware topology. Over time, it may have
// RingBufferInspectInstance children, if a client connects to the RingBuffer API.
class RingBufferElement {
 public:
  RingBufferElement(inspect::Node ring_buffer_element_node, ElementId element_id);
  ~RingBufferElement();

  inspect::Node& inspect_node() { return ring_buffer_element_node_; }
  std::shared_ptr<RingBufferInspectInstance> RecordRingBufferInstance(const zx::time& created_at);
  ElementId element_id() const { return element_id_; }

 private:
  static inline const std::string_view kClassName = "RingBufferElement";

  inspect::Node ring_buffer_element_node_;
  ElementId element_id_;
  inspect::UintProperty element_id_property_;

  std::vector<std::shared_ptr<RingBufferInspectInstance>> ring_buffer_instances_;
};

// This represents an audio driver and its device. It is created when an audio device is detected in
// DevFs or added via Provider/AddDevice.
class DeviceInspectInstance {
 public:
  DeviceInspectInstance(inspect::Node device_node, std::string device_name,
                        fuchsia_audio_device::DeviceType device_type, const zx::time& added_at);
  ~DeviceInspectInstance();

  inspect::Node& inspect_node() { return device_node_; }

  void RecordTokenId(TokenId token_id);
  void RecordDeviceHealthOk();
  void RecordProperties(std::optional<bool> is_input, std::optional<std::string> manufacturer,
                        std::optional<std::string> product,
                        std::optional<std::string> unique_instance_id,
                        std::optional<ClockDomain> clock_domain);
  std::shared_ptr<RingBufferElement> RecordRingBufferElement(ElementId element_id);
  std::shared_ptr<RingBufferInspectInstance> RecordRingBufferInstance(ElementId element_id,
                                                                      const zx::time& created_at);

  void RecordError(const zx::time& failed_at);
  void RecordRemoval(const zx::time& removed_at);

 private:
  static inline const std::string_view kClassName = "DeviceInspectInstance";

  inspect::Node device_node_;
  std::string name_;
  inspect::IntProperty added_at_;
  inspect::StringProperty type_;

  inspect::Node ring_buffers_root_node_;
  inspect::UintProperty token_id_;
  inspect::BoolProperty is_input_;
  inspect::StringProperty manufacturer_;
  inspect::StringProperty product_;
  inspect::StringProperty unique_instance_id_;
  inspect::UintProperty clock_domain_;

  inspect::BoolProperty healthy_;
  inspect::IntProperty failed_at_;
  inspect::IntProperty removed_at_;

  std::vector<std::shared_ptr<RingBufferElement>> ring_buffer_elements_;
};

// This represents a client connection to one of the six ADR FIDL protocols:
// Registry, Observer, ControlCreator, Control, RingBuffer or Provider.
class FidlServerInspectInstance {
 public:
  FidlServerInspectInstance(inspect::Node instance_node, const zx::time& created_at);
  ~FidlServerInspectInstance();

  void RecordDestructionTime(const zx::time& destroyed_at);

 protected:
  inspect::Node& instance_node() { return instance_node_; }

 private:
  static inline const std::string_view kClassName = "FidlServerInspectInstance";

  inspect::Node instance_node_;
  inspect::IntProperty created_at_;
  inspect::IntProperty destroyed_at_;
};

// We save additional information or each client Provider instance: the devices that it has added.
// We reuse DeviceInspectInstance but use only a subset (name, type, added_at).
class ProviderInspectInstance : public FidlServerInspectInstance {
 public:
  ProviderInspectInstance(inspect::Node provider_instance_node, const zx::time& created_at);
  ~ProviderInspectInstance();

  void RecordAddedDevice(const std::string& device_name,
                         const fuchsia_audio_device::DeviceType& device_type,
                         const zx::time& added_at);

 private:
  static inline const std::string_view kClassName = "ProviderInspectInstance";

  inspect::Node provider_devices_root_node_;
  std::vector<std::shared_ptr<DeviceInspectInstance>> provider_devices_;
};

// This singleton manages (and owns, from a lifetime mgmt standpoint) Inspect for the entire ADR.
// Inspect for core/audio_device_registry contains two sections: Devices and FIDL Servers.
// The Devices section contains info on devices that have been detected and presented to clients.
// The FIDL Servers section contains info on client instances of the six ADR FIDL protocols.
class Inspector {
 public:
  static void Initialize(async_dispatcher_t* dispatcher);
  static std::unique_ptr<Inspector>& Singleton() { return singleton_; }
  std::unique_ptr<inspect::ComponentInspector>& component_inspector() {
    return component_inspector_;
  }

  explicit Inspector(async_dispatcher_t* dispatcher);
  ~Inspector();

  void RecordHealthOk();
  void RecordUnhealthy(const std::string& health_message);

  void RecordDetectionFailureToConnect();
  void RecordDetectionFailureOther();
  void RecordDetectionFailureUnsupported();

  std::shared_ptr<DeviceInspectInstance> RecordDeviceInitializing(
      const std::string& device_name, fuchsia_audio_device::DeviceType device_type,
      const zx::time& added_at);

  // Create an Inspect node for the instance (e.g. a child of control_servers_root_ if this is a
  // Control instance), wrap it in a FidlServerInspectInstance object, and return a shared_ptr to
  // it. This class (via control_server_instances_ etc.) owns the instance node; the child
  // FidlServerInspectInstance creates nodes or properties attached to the instance node.
  std::shared_ptr<FidlServerInspectInstance> RecordRegistryInstance(const zx::time& created_at);
  std::shared_ptr<FidlServerInspectInstance> RecordObserverInstance(const zx::time& created_at);
  std::shared_ptr<FidlServerInspectInstance> RecordControlCreatorInstance(
      const zx::time& created_at);
  std::shared_ptr<FidlServerInspectInstance> RecordControlInstance(const zx::time& created_at);
  std::shared_ptr<FidlServerInspectInstance> RecordRingBufferInstance(const zx::time& created_at);
  std::shared_ptr<ProviderInspectInstance> RecordProviderInspectInstance(
      const zx::time& created_at);

 private:
  static inline const std::string_view kClassName = "Inspector";

  static std::unique_ptr<Inspector> singleton_;

  std::unique_ptr<inspect::ComponentInspector> component_inspector_;
  inspect::Node& inspect_root_;

  inspect::UintProperty count_devices_failed_to_connect_;
  inspect::UintProperty count_device_watcher_failures_;
  inspect::UintProperty count_detected_unsupported_device_type_;

  // The top-level "Devices" parent node
  inspect::Node devices_root_;

  // Each entry in this vector represents an audio driver and device.
  // Note that entries are not removed when a device encounters an error or is removed.
  std::vector<std::shared_ptr<DeviceInspectInstance>> device_instances_;

  // The top-level "FIDL Servers" parent node
  inspect::Node fidl_servers_root_;

  inspect::Node registry_servers_root_;
  inspect::Node observer_servers_root_;
  inspect::Node control_creator_servers_root_;
  inspect::Node control_servers_root_;
  inspect::Node ring_buffer_servers_root_;
  inspect::Node provider_servers_root_;

  // Each entry in these vectors represents a client instance of one of the ADR protocols and
  // includes an Inspect node for that instance as well as timestamps for creation and destruction.
  // Note that entries are not removed when a client disconnects.
  std::vector<std::shared_ptr<FidlServerInspectInstance>> registry_server_instances_;
  std::vector<std::shared_ptr<FidlServerInspectInstance>> observer_server_instances_;
  std::vector<std::shared_ptr<FidlServerInspectInstance>> control_creator_server_instances_;
  std::vector<std::shared_ptr<FidlServerInspectInstance>> control_server_instances_;
  std::vector<std::shared_ptr<FidlServerInspectInstance>> ring_buffer_server_instances_;
  std::vector<std::shared_ptr<ProviderInspectInstance>> provider_server_instances_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_INSPECTOR_H_
