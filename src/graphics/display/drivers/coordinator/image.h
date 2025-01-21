// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_IMAGE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_IMAGE_H_

#include <lib/inspect/cpp/inspect.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <atomic>

#include <fbl/intrusive_container_utils.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display_coordinator {

class Controller;

// An Image is both a reference to an imported pixel buffer (hereafter ImageRef)
// and the state machine (hereafter ImageUse) for tracking its use as part of a config.
//
// ImageUse can be NOT_READY, READY, ACQUIRED, or PRESENTED.
//   NOT_READY: initial state, transitions to READY when wait_event is null or signaled.
//              When returning to NOT_READY via EarlyRetire, the signal_fence will fire.
//   READY: the related ImageRef is ready for use. Controller::ApplyConfig may request a
//          move to ACQUIRED (Acquire) or NOT_READY (EarlyRetire) because another ImageUse
//          was ACQUIRED instead.
//   ACQUIRED: this image will be used on the next display flip. Transitions to PRESENTED
//             when the display hardware reports it in OnVsync.
//   PRESENTED: this image has been observed in OnVsync. Transitions to NOT_READY when
//              the Controller determines that a new ImageUse has been PRESENTED and
//              this one can be retired.
//
// One special transition exists: upon the owning Client's death/disconnection, the
// ImageUse will move from ACQUIRED to NOT_READY.
class Image : public fbl::RefCounted<Image>,
              public IdMappable<fbl::RefPtr<Image>, display::ImageId> {
 private:
  // Private forward declaration.
  template <typename PtrType, typename TagType>
  struct DoublyLinkedListTraits;

  // Private typename aliases for DoublyLinkedList definition.
  using DoublyLinkedListPointer = fbl::RefPtr<Image>;
  using DefaultDoublyLinkedListTraits =
      DoublyLinkedListTraits<DoublyLinkedListPointer, fbl::DefaultObjectTag>;

 public:
  // This defines the specific type of fbl::DoublyLinkedList that an Image can
  // be placed into. Any intrusive container that can hold an Image must be of
  // type Image::DoublyLinkedList.
  //
  // Note that the default fbl::DoublyLinkedList doesn't work in this case, due
  // to the intrusive linked list node is guarded by a mutex.
  using DoublyLinkedList = fbl::DoublyLinkedList<DoublyLinkedListPointer, fbl::DefaultObjectTag,
                                                 fbl::SizeOrder::N, DefaultDoublyLinkedListTraits>;

  // `controller` must be non-null, and must outlive the Image.
  Image(Controller* controller, const display::ImageMetadata& metadata,
        display::DriverImageId driver_id, inspect::Node* parent_node, ClientId client_id);
  ~Image();

  display::DriverImageId driver_id() const { return driver_id_; }
  const display::ImageMetadata& metadata() const { return metadata_; }

  // The client that owns the image.
  ClientId client_id() const { return client_id_; }

  // Marks the image as in use.
  bool Acquire();
  // Marks the image as not in use. Should only be called before PrepareFences.
  void DiscardAcquire();
  // Prepare the image for display. It will not be READY until `wait` is signaled.
  void PrepareFences(fbl::RefPtr<FenceReference>&& wait);
  // Called to immediately retire the image if StartPresent hasn't been called yet.
  void EarlyRetire();
  // Called when the image is passed to the display hardware.
  void StartPresent() __TA_REQUIRES(mtx());
  // Called when another image is presented after this one.
  void StartRetire() __TA_REQUIRES(mtx());
  // Called on vsync after StartRetire has been called.
  void OnRetire() __TA_REQUIRES(mtx());

  // Called on all waiting images when any fence fires. Returns true if the image is ready to
  // present.
  bool OnFenceReady(FenceReference* fence);

  // Called to reset fences when client releases the image. Releasing fences
  // is independent of the rest of the image lifecycle.
  void ResetFences() __TA_REQUIRES(mtx());

  bool IsReady() const { return wait_fence_ == nullptr; }

  void set_latest_controller_config_stamp(display::ConfigStamp stamp) {
    latest_controller_config_stamp_ = stamp;
  }
  display::ConfigStamp latest_controller_config_stamp() const {
    return latest_controller_config_stamp_;
  }

  void set_latest_client_config_stamp(display::ConfigStamp stamp) {
    latest_client_config_stamp_ = stamp;
  }
  display::ConfigStamp latest_client_config_stamp() const { return latest_client_config_stamp_; }

  // Aliases controller_.mtx() for the purpose of thread-safety analysis.
  fbl::Mutex* mtx() const;

  // Checks if the Image is in a DoublyLinkedList container.
  bool InDoublyLinkedList() const __TA_REQUIRES(mtx());

  // Removes the Image from the DoublyLinkedList. The Image must be in a
  // DoublyLinkedList when this is called.
  DoublyLinkedListPointer RemoveFromDoublyLinkedList() __TA_REQUIRES(mtx());

 private:
  // This defines the node trait used by the fbl::DoublyLinkedList that an Image
  // can be placed in. PointerType and TagType are required for template
  // argument resolution purpose in `fbl::DoublyLinkedList`.
  template <typename PointerType, typename TagType>
  struct DoublyLinkedListTraits {
   public:
    static auto& node_state(Image& obj) { return obj.doubly_linked_list_node_state_; }
  };
  friend DoublyLinkedListTraits<DoublyLinkedListPointer, fbl::DefaultObjectTag>;

  void InitializeInspect(inspect::Node* parent_node);

  // This NodeState allows the Image to be placed in an intrusive
  // Image::DoublyLinkedList which can be either a Client's waiting image
  // list, or the Controller's presented image list.
  //
  // The presented image list is protected with the controller mutex, and the
  // waiting list is only accessed on the loop and thus is not generally
  // protected. However, transfers between the lists are protected by the
  // controller mutex.
  fbl::DoublyLinkedListNodeState<DoublyLinkedListPointer,
                                 fbl::NodeOptions::AllowRemoveFromContainer>
      doubly_linked_list_node_state_ __TA_GUARDED(mtx());

  const display::DriverImageId driver_id_;
  const display::ImageMetadata metadata_;

  Controller& controller_;
  const ClientId client_id_;

  // Stamp of the latest Controller display configuration that uses this image.
  display::ConfigStamp latest_controller_config_stamp_ = display::kInvalidConfigStamp;

  // Stamp of the latest display configuration in Client (the DisplayController
  // FIDL service) that uses this image.
  //
  // Note that for an image, it is possible that its |latest_client_config_stamp_|
  // doesn't match the |latest_controller_config_stamp_|. This could happen when
  // a client configuration sets a new layer image but the new image is not
  // ready yet, so the controller has to keep using the old image.
  display::ConfigStamp latest_client_config_stamp_ = display::kInvalidConfigStamp;

  // Indicates that the image contents are ready for display.
  // Only ever accessed on loop thread, so no synchronization
  fbl::RefPtr<FenceReference> wait_fence_ = nullptr;

  // Flag which indicates that the image is currently in some display configuration.
  std::atomic_bool in_use_ = {};
  // Flag indicating that the image is being managed by the display hardware.
  bool presenting_ __TA_GUARDED(mtx()) = false;
  // Flag indicating that the image has started the process of retiring and will be free after
  // the next vsync. This is distinct from presenting_ due to multiplexing the display between
  // multiple clients.
  bool retiring_ __TA_GUARDED(mtx()) = false;

  inspect::Node node_;
  inspect::ValueList properties_;
  inspect::BoolProperty presenting_property_;
  inspect::BoolProperty retiring_property_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_IMAGE_H_
