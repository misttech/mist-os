// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_IMAGE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_IMAGE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <atomic>

#include <fbl/intrusive_container_utils.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

namespace display {

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
              public IdMappable<fbl::RefPtr<Image>, /*IdType=*/uint64_t> {
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

  Image(Controller* controller, const image_t& info, zx::vmo vmo, inspect::Node* parent_node,
        uint32_t client_id);
  ~Image();

  image_t& info() { return info_; }
  uint32_t client_id() const { return client_id_; }

  // Marks the image as in use.
  bool Acquire();
  // Marks the image as not in use. Should only be called before PrepareFences.
  void DiscardAcquire();
  // Prepare the image for display. It will not be READY until `wait` is
  // signaled, and once the image is no longer displayed `retire` will be signaled.
  void PrepareFences(fbl::RefPtr<FenceReference>&& wait, fbl::RefPtr<FenceReference>&& retire);
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

  // True iff the image has the same display properties as the `layer_config`,
  // which are properties of all images that the corresponding Layer can accept.
  bool HasSameDisplayPropertiesAsLayer(const image_t& layer_config) const;

  const zx::vmo& vmo() { return vmo_; }

  void set_latest_controller_config_stamp(ConfigStamp stamp) {
    latest_controller_config_stamp_ = stamp;
  }
  ConfigStamp latest_controller_config_stamp() const { return latest_controller_config_stamp_; }

  void set_latest_client_config_stamp(ConfigStamp stamp) { latest_client_config_stamp_ = stamp; }
  ConfigStamp latest_client_config_stamp() const { return latest_client_config_stamp_; }

  // Aliases controller_->mtx() for the purpose of thread-safety analysis.
  mtx_t* mtx() const;

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

  // Retires the image and signals |fence|.
  void RetireWithFence(fbl::RefPtr<FenceReference>&& fence);
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

  image_t info_;

  Controller* const controller_;

  // |id_| of the client that created the image.
  const uint32_t client_id_;

  // Stamp of the latest Controller display configuration that uses this image.
  ConfigStamp latest_controller_config_stamp_ = kInvalidConfigStamp;

  // Stamp of the latest display configuration in Client (the DisplayController
  // FIDL service) that uses this image.
  //
  // Note that for an image, it is possible that its |latest_client_config_stamp_|
  // doesn't match the |latest_controller_config_stamp_|. This could happen when
  // a client configuration sets a new layer image but the new image is not
  // ready yet, so the controller has to keep using the old image.
  ConfigStamp latest_client_config_stamp_ = kInvalidConfigStamp;

  // Indicates that the image contents are ready for display.
  // Only ever accessed on loop thread, so no synchronization
  fbl::RefPtr<FenceReference> wait_fence_ = nullptr;

  // retire_fence_ is signaled when an image is no longer used on a display.
  // retire_fence_ is only accessed on the loop. armed_retire_fence_ is accessed
  // under the controller mutex. See comment in ::OnRetire for more details.
  // All retires are performed by the Controller's ApplyConfig/OnDisplayVsync loop.
  fbl::RefPtr<FenceReference> retire_fence_ = nullptr;
  fbl::RefPtr<FenceReference> armed_retire_fence_ __TA_GUARDED(mtx()) = nullptr;

  // Flag which indicates that the image is currently in some display configuration.
  std::atomic_bool in_use_ = {};
  // Flag indicating that the image is being managed by the display hardware.
  bool presenting_ __TA_GUARDED(mtx()) = false;
  // Flag indicating that the image has started the process of retiring and will be free after
  // the next vsync. This is distinct from presenting_ due to multiplexing the display between
  // multiple clients.
  bool retiring_ __TA_GUARDED(mtx()) = false;

  const zx::vmo vmo_;

  inspect::Node node_;
  inspect::ValueList properties_;
  inspect::BoolProperty presenting_property_;
  inspect::BoolProperty retiring_property_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_IMAGE_H_
