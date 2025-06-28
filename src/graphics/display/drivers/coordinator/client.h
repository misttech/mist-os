// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/zx/time.h>
#include <zircon/types.h>

#include <cstdint>
#include <map>
#include <optional>
#include <span>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/ref_ptr.h>
#include <fbl/ring_buffer.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/capture-image.h"
#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/display-config.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/layer.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-id.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

namespace display_coordinator {

class ClientProxy;

// Manages the state associated with a display coordinator client connection.
//
// This class is not thread-safe. After initialization, all methods must be
// executed on the same thread.
class Client final : public fidl::WireServer<fuchsia_hardware_display::Coordinator> {
 public:
  // `controller` must outlive both this client and `proxy`.
  Client(Controller* controller, ClientProxy* proxy, ClientPriority priority, ClientId client_id);

  Client(const Client&) = delete;
  Client& operator=(const Client&) = delete;

  ~Client() override;

  // Binds the `Client` to the server-side channel of the `Coordinator`
  // protocol.
  //
  // Must be called exactly once in production code.
  //
  // `coordinator_server_end` and `coordinator_listener_client_end` must be valid.
  void Bind(fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
            fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
                coordinator_listener_client_end,
            fidl::OnUnboundFn<Client> unbound_callback);

  void OnDisplaysChanged(std::span<const display::DisplayId> added_display_ids,
                         std::span<const display::DisplayId> removed_display_ids);
  void SetOwnership(bool is_owner);

  fidl::Status NotifyDisplayChanges(
      std::span<const fuchsia_hardware_display::wire::Info> added_display_infos,
      std::span<const fuchsia_hardware_display_types::wire::DisplayId> removed_display_ids);
  fidl::Status NotifyOwnershipChange(bool client_has_ownership);
  fidl::Status NotifyVsync(display::DisplayId display_id, zx::time timestamp,
                           display::ConfigStamp config_stamp,
                           display::VsyncAckCookie vsync_ack_cookie);

  // Applies a previously applied configuration.
  //
  // Called when a client regains ownership of the displays.
  //
  // This method is a no-op if the client has not applied any configuration.
  void ReapplyConfig();

  void OnFenceFired(FenceReference* fence);

  void TearDown(zx_status_t epitaph);
  void TearDownForTesting();

  bool IsValid() const { return valid_; }
  ClientId id() const { return id_; }
  ClientPriority priority() const { return priority_; }
  void CaptureCompleted();

  uint8_t GetMinimumRgb() const { return client_minimum_rgb_; }

  display::VsyncAckCookie LastAckedCookie() const { return acked_cookie_; }

  size_t ImportedImagesCountForTesting() const { return images_.size(); }

  // fidl::WireServer<fuchsia_hardware_display::Coordinator> overrides:
  void ImportImage(ImportImageRequestView request, ImportImageCompleter::Sync& _completer) override;
  void ReleaseImage(ReleaseImageRequestView request,
                    ReleaseImageCompleter::Sync& _completer) override;
  void ImportEvent(ImportEventRequestView request, ImportEventCompleter::Sync& _completer) override;
  void ReleaseEvent(ReleaseEventRequestView request,
                    ReleaseEventCompleter::Sync& _completer) override;
  void CreateLayer(CreateLayerCompleter::Sync& _completer) override;
  void DestroyLayer(DestroyLayerRequestView request,
                    DestroyLayerCompleter::Sync& _completer) override;
  void SetDisplayMode(SetDisplayModeRequestView request,
                      SetDisplayModeCompleter::Sync& _completer) override;
  void SetDisplayColorConversion(SetDisplayColorConversionRequestView request,
                                 SetDisplayColorConversionCompleter::Sync& _completer) override;
  void SetDisplayLayers(SetDisplayLayersRequestView request,
                        SetDisplayLayersCompleter::Sync& _completer) override;
  void SetLayerPrimaryConfig(SetLayerPrimaryConfigRequestView request,
                             SetLayerPrimaryConfigCompleter::Sync& _completer) override;
  void SetLayerPrimaryPosition(SetLayerPrimaryPositionRequestView request,
                               SetLayerPrimaryPositionCompleter::Sync& _completer) override;
  void SetLayerPrimaryAlpha(SetLayerPrimaryAlphaRequestView request,
                            SetLayerPrimaryAlphaCompleter::Sync& _completer) override;
  void SetLayerColorConfig(SetLayerColorConfigRequestView request,
                           SetLayerColorConfigCompleter::Sync& _completer) override;
  void SetLayerImage2(SetLayerImage2RequestView request,
                      SetLayerImage2Completer::Sync& _completer) override;
  void CheckConfig(CheckConfigCompleter::Sync& _completer) override;
  void DiscardConfig(DiscardConfigCompleter::Sync& _completer) override;
  void ApplyConfig3(ApplyConfig3RequestView request,
                    ApplyConfig3Completer::Sync& _completer) override;
  void GetLatestAppliedConfigStamp(GetLatestAppliedConfigStampCompleter::Sync& _completer) override;
  void SetVsyncEventDelivery(SetVsyncEventDeliveryRequestView request,
                             SetVsyncEventDeliveryCompleter::Sync& _completer) override;
  void SetVirtconMode(SetVirtconModeRequestView request,
                      SetVirtconModeCompleter::Sync& _completer) override;
  void ImportBufferCollection(ImportBufferCollectionRequestView request,
                              ImportBufferCollectionCompleter::Sync& _completer) override;
  void SetBufferCollectionConstraints(
      SetBufferCollectionConstraintsRequestView request,
      SetBufferCollectionConstraintsCompleter::Sync& _completer) override;
  void ReleaseBufferCollection(ReleaseBufferCollectionRequestView request,
                               ReleaseBufferCollectionCompleter::Sync& _completer) override;

  void IsCaptureSupported(IsCaptureSupportedCompleter::Sync& _completer) override;

  void StartCapture(StartCaptureRequestView request,
                    StartCaptureCompleter::Sync& _completer) override;

  void AcknowledgeVsync(AcknowledgeVsyncRequestView request,
                        AcknowledgeVsyncCompleter::Sync& _completer) override;

  void SetMinimumRgb(SetMinimumRgbRequestView request,
                     SetMinimumRgbCompleter::Sync& _completer) override;

  void SetDisplayPower(SetDisplayPowerRequestView request,
                       SetDisplayPowerCompleter::Sync& _completer) override;

 private:
  display::ConfigCheckResult CheckConfigImpl();
  void ApplyConfigImpl();

  // CheckConfig() implementation for a single display configuration.
  //
  // `display_config`'s draft configuration must have a non-empty layer list.
  display::ConfigCheckResult CheckConfigForDisplay(const DisplayConfig& display_config);

  // Cleans up states of all current Images.
  // Returns true if any current layer has been modified.
  bool CleanUpAllImages();

  // Cleans up layer state associated with an Image. `image` must be valid.
  // Returns true if a current layer has been modified.
  bool CleanUpImage(Image& image);
  void CleanUpCaptureImage(display::ImageId id);

  // Displays' draft layers list may have been changed by SetDisplayLayers().
  //
  // Restores the draft layer lists of all the displays to their applied layer
  // list state respectively, undoing all draft changes to the layer lists.
  void SetAllConfigDraftLayersToAppliedLayers();

  // `fuchsia.hardware.display/Coordinator.ImportImage()` helper for display
  // images.
  //
  // `image_id` must be unused and `image_metadata` contains metadata for an
  // image used for display.
  zx_status_t ImportImageForDisplay(const display::ImageMetadata& image_metadata,
                                    display::BufferId buffer_id, display::ImageId image_id);

  // `fuchsia.hardware.display/Coordinator.ImportImage()` helper for capture
  // images.
  //
  // `image_id` must be unused and `image_metadata` contains metadata for an
  // image used for capture.
  zx_status_t ImportImageForCapture(const display::ImageMetadata& image_metadata,
                                    display::BufferId buffer_id, display::ImageId image_id);

  // Discards all the draft configs on all displays and layers.
  void DiscardConfig();

  void SetLayerImageImpl(display::LayerId layer_id, display::ImageId image_id,
                         display::EventId wait_event_id);

  Controller& controller_;
  ClientProxy* const proxy_;
  const ClientPriority priority_;
  const ClientId id_;
  bool valid_ = false;

  Image::Map images_;
  CaptureImage::Map capture_images_;

  // Maps each known display ID to this client's display config.
  //
  // The client's knowledge of the connected displays can fall out of sync with
  // this map. This is because the map is modified when the Coordinator
  // processes display change events from display engine drivers, which happens
  // before the client receives the display driver.
  DisplayConfig::Map display_configs_;

  // True iff CheckConfig() succeeded on the draft configuration.
  //
  // Set to false any time when the client modifies the draft configuration. Set
  // to true when the client calls CheckConfig() and the check passes.
  bool draft_display_config_was_validated_ = false;

  bool is_owner_ = false;

  // A counter for the number of times the client has successfully applied
  // a configuration. This does not account for changes due to waiting images.
  display::ConfigStamp latest_config_stamp_ = display::kInvalidConfigStamp;

  // This is the client's clamped RGB value.
  uint8_t client_minimum_rgb_ = 0;

  struct Collections {
    // The BufferCollection ID used in fuchsia.hardware.display.Controller
    // protocol.
    display::DriverBufferCollectionId driver_buffer_collection_id;
  };
  std::map<display::BufferCollectionId, Collections> collection_map_;

  FenceCollection fences_;

  Layer::Map layers_;

  // TODO(fxbug.com/129082): Move to Controller, so values issued using this
  // counter are globally unique. Do not pass to display::DriverLayerId values to drivers
  // until this issue is fixed.
  display::DriverLayerId next_driver_layer_id = display::DriverLayerId(1);

  void NotifyDisplaysChanged(const int32_t* displays_added, uint32_t added_count,
                             const int32_t* displays_removed, uint32_t removed_count);

  std::optional<fidl::ServerBindingRef<fuchsia_hardware_display::Coordinator>> binding_;
  fidl::WireSharedClient<fuchsia_hardware_display::CoordinatorListener> coordinator_listener_;

  // Capture related bookkeeping.
  display::EventId capture_fence_id_ = display::kInvalidEventId;

  // Points to the image whose contents is modified by the current capture.
  //
  // Invalid when no is capture in progress.
  display::ImageId current_capture_image_id_ = display::kInvalidImageId;

  // Tracks an image released by the client while used by a capture.
  //
  // The coordinator must ensure that an image remains valid while a display
  // engine is writing to it. If a client attempts to release the image used by
  // an in-progress capture, we defer the release operation until the capture
  // completes. The deferred release is tracked here.
  display::ImageId pending_release_capture_image_id_ = display::kInvalidImageId;

  display::VsyncAckCookie acked_cookie_ = display::kInvalidVsyncAckCookie;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_H_
