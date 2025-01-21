// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_FENCE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_FENCE_H_

#include <lib/async/cpp/wait.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/zx/event.h>
#include <threads.h>
#include <zircon/compiler.h>

#include <fbl/intrusive_double_list.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/macros.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"

namespace display_coordinator {

class FenceReference;
class Fence;

class FenceCallback {
 public:
  virtual void OnFenceFired(FenceReference* ref) = 0;
  virtual void OnRefForFenceDead(Fence* fence) = 0;
};

// Class which wraps an event into a fence. A single Fence can have multiple FenceReference
// objects, which allows an event to be treated as a semaphore independently of it being
// imported/released (i.e. can be released while still in use).
//
// Fence is not thread-safe (but thread-compatible). For the sake of simplicity,
// in order to avoid data races, we require `Fence`s and its `FenceReference`s
// be created and destroyed on the same fdf::Dispatcher where the Fence is
// created.
class Fence : public fbl::RefCounted<Fence>,
              public IdMappable<fbl::RefPtr<Fence>, display::EventId>,
              public fbl::SinglyLinkedListable<fbl::RefPtr<Fence>> {
 public:
  // `Fence` must be created on a dispatcher managed by the driver framework.
  // The dispatcher must be valid throughout the lifetime of the `Fence`.
  //
  // `event_dispatcher` is where the asynchronous events regarding this Fence
  // are dispatched. It may be the same as the dispatcher where the Fence is
  // created.
  //
  // `event_dispatcher` must not be null and must outlive Fence.
  Fence(FenceCallback* cb, async_dispatcher_t* event_dispatcher, display::EventId id,
        zx::event event);
  ~Fence();

  Fence(const Fence& other) = delete;
  Fence(Fence&& other) = delete;
  Fence& operator=(const Fence& other) = delete;
  Fence& operator=(Fence&& other) = delete;

  // Creates a new FenceReference when an event is imported.
  bool CreateRef();
  // Clears a FenceReference when an event is released. Note that references to the cleared
  // FenceReference might still exist within the driver.
  void ClearRef();
  // Decrements the reference count and returns true if the last ref died.
  bool OnRefDead();

  // Gets the fence reference for the current import. An individual fence reference cannot
  // be used for multiple things simultaneously.
  fbl::RefPtr<FenceReference> GetReference();

  // The raw event underlying this fence. Only used for validation.
  zx_handle_t event() const { return event_.get(); }

 private:
  void Signal() const;
  void OnRefDied();
  zx_status_t OnRefArmed(fbl::RefPtr<FenceReference>&& ref);
  void OnRefDisarmed(FenceReference* ref);

  // The fence reference corresponding to the current event import.
  fbl::RefPtr<FenceReference> cur_ref_;

  // A queue of fence references which are being waited upon. When the event is
  // signaled, the signal will be cleared and the first fence ref will be marked ready.
  fbl::DoublyLinkedList<fbl::RefPtr<FenceReference>> armed_refs_;

  void OnReady(async_dispatcher_t* dispatcher, async::WaitBase* self, zx_status_t status,
               const zx_packet_signal_t* signal);
  async::WaitMethod<Fence, &Fence::OnReady> ready_wait_{this};

  FenceCallback* cb_;

  async_dispatcher_t& event_dispatcher_;
  fdf::UnownedDispatcher const fence_creation_dispatcher_;

  zx::event event_;
  int ref_count_ = 0;
  zx_koid_t koid_ = 0;

  friend FenceReference;
};

// Each FenceReference represents a pending / active wait or signaling of the
// Fence it refers to, regardless of the Fence it refers to being imported
// or released by the Client.
//
// FenceReference is not thread-safe (but thread-compatible). For the sake of
// simplicity, we require `FenceReference`s be created and destroyed on the same
// fdf::Dispatcher where the Fence is created.
class FenceReference : public fbl::RefCounted<FenceReference>,
                       public fbl::DoublyLinkedListable<fbl::RefPtr<FenceReference>> {
 public:
  // `FenceReference` must be created on `fence_creation_dispatcher`, which is
  // the dispatcher where `fence` is created.
  explicit FenceReference(fbl::RefPtr<Fence> fence,
                          fdf::UnownedDispatcher fence_creation_dispatcher);
  ~FenceReference();

  FenceReference(const FenceReference& other) = delete;
  FenceReference(FenceReference&& other) = delete;
  FenceReference& operator=(const FenceReference& other) = delete;
  FenceReference& operator=(FenceReference&& other) = delete;

  void Signal() const;

  zx_status_t StartReadyWait();
  void ResetReadyWait();

 private:
  fbl::RefPtr<Fence> fence_;

  fdf::UnownedDispatcher const fence_creation_dispatcher_;
};

// FenceCollection controls the access and lifecycles for several display::Fences.
class FenceCollection : private FenceCallback {
 public:
  // Creates an empty collection.
  //
  // Fence events are dispatched on `dispatcher`.
  // `dispatcher` must be non-null and must outlive the newly created instance.
  //
  // `on_fence_fired` must be callable while the newly created instance is
  // alive.
  //
  // `on_fence_fired` will be called when one of the fences fires. The call
  // will be done from an async task processed using `dispatcher`.
  FenceCollection(async_dispatcher_t* dispatcher,
                  fit::function<void(FenceReference*)> on_fence_fired);

  FenceCollection(const FenceCollection&) = delete;
  FenceCollection(FenceCollection&&) = delete;
  FenceCollection& operator=(const FenceCollection&) = delete;
  FenceCollection& operator=(FenceCollection&&) = delete;

  virtual ~FenceCollection() = default;

  // Explicit destruction step. Use this to control when fences are destroyed.
  void Clear() __TA_EXCLUDES(mtx_);

  // Import `event` so that it can subsequently be referenced by passing `id` to `GetFence()`.
  // `id` must not already be registered by a previous call to `ImportEvent()`, unless it was
  // subsequently unregistered by calling `ReleaseEvent()`.
  zx_status_t ImportEvent(zx::event event, display::EventId id) __TA_EXCLUDES(mtx_);

  // Unregister a fence that was previously registered by `ImportEvent()`.
  void ReleaseEvent(display::EventId id) __TA_EXCLUDES(mtx_);

  // Get reference to existing fence by its ID, or nullptr if no fence is found.
  fbl::RefPtr<FenceReference> GetFence(display::EventId id) __TA_EXCLUDES(mtx_);

 private:
  // |FenceCallback|
  void OnFenceFired(FenceReference* fence) override;

  // |FenceCallback|
  void OnRefForFenceDead(Fence* fence) __TA_EXCLUDES(mtx_) override;

  fbl::Mutex mtx_;
  Fence::Map fences_ __TA_GUARDED(mtx_);
  async_dispatcher_t* const dispatcher_;
  fit::function<void(FenceReference*)> on_fence_fired_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_FENCE_H_
