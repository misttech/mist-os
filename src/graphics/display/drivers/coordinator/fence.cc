// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/fence.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/fit/thread_checker.h>
#include <zircon/assert.h>

#include <thread>
#include <utility>

#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/lib/api-types-cpp/event-id.h"

namespace display {

bool Fence::CreateRef() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  fbl::AllocChecker ac;
  cur_ref_ =
      fbl::AdoptRef(new (&ac) FenceReference(fbl::RefPtr<Fence>(this), std::this_thread::get_id()));
  if (ac.check()) {
    ref_count_++;
  }

  return ac.check();
}

void Fence::ClearRef() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  cur_ref_ = nullptr;
}

fbl::RefPtr<FenceReference> Fence::GetReference() { return cur_ref_; }

void Fence::Signal() const { event_.signal(0, ZX_EVENT_SIGNALED); }

bool Fence::OnRefDead() { return --ref_count_ == 0; }

zx_status_t Fence::OnRefArmed(fbl::RefPtr<FenceReference>&& ref) {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);

  if (armed_refs_.is_empty()) {
    ready_wait_.set_object(event_.get());
    ready_wait_.set_trigger(ZX_EVENT_SIGNALED);

    zx_status_t status = ready_wait_.Begin(dispatcher_);
    if (status != ZX_OK) {
      return status;
    }
  }

  armed_refs_.push_back(std::move(ref));
  return ZX_OK;
}

void Fence::OnRefDisarmed(FenceReference* ref) {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);

  armed_refs_.erase(*ref);
  if (armed_refs_.is_empty()) {
    ready_wait_.Cancel();
  }
}

void Fence::OnReady(async_dispatcher_t* dispatcher, async::WaitBase* self, zx_status_t status,
                    const zx_packet_signal_t* signal) {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);

  ZX_DEBUG_ASSERT(status == ZX_OK && (signal->observed & ZX_EVENT_SIGNALED));
  TRACE_DURATION("gfx", "Display::Fence::OnReady");
  TRACE_FLOW_END("gfx", "event_signal", koid_);

  event_.signal(ZX_EVENT_SIGNALED, 0);

  fbl::RefPtr<FenceReference> ref = armed_refs_.pop_front();
  ref->OnReady();
  cb_->OnFenceFired(ref.get());

  if (!armed_refs_.is_empty()) {
    ready_wait_.Begin(dispatcher_);
  }
}

Fence::Fence(FenceCallback* cb, async_dispatcher_t* dispatcher, EventId fence_id, zx::event&& event)
    : cb_(cb), dispatcher_(dispatcher), event_(std::move(event)) {
  id = fence_id;
  ZX_DEBUG_ASSERT(event_.is_valid());
  zx_info_handle_basic_t info;
  zx_status_t status = event_.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  ZX_DEBUG_ASSERT(status == ZX_OK);
  koid_ = info.koid;
}

Fence::~Fence() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  ZX_DEBUG_ASSERT(armed_refs_.is_empty());
  ZX_DEBUG_ASSERT(ref_count_ == 0);
}

zx_status_t FenceReference::StartReadyWait() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  return fence_->OnRefArmed(fbl::RefPtr<FenceReference>(this));
}

void FenceReference::ResetReadyWait() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  fence_->OnRefDisarmed(this);
}

void FenceReference::SetImmediateRelease(fbl::RefPtr<FenceReference>&& fence) {
  release_fence_ = std::move(fence);
}

void FenceReference::OnReady() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  if (release_fence_) {
    release_fence_->Signal();
    release_fence_ = nullptr;
  }
}

void FenceReference::Signal() const { fence_->Signal(); }

FenceReference::FenceReference(fbl::RefPtr<Fence> fence, std::thread::id fence_thread_id)
    : fence_(std::move(fence)), thread_checker_(fence_thread_id) {}

FenceReference::~FenceReference() {
  FIT_DCHECK_IS_THREAD_VALID(thread_checker_);
  fence_->cb_->OnRefForFenceDead(fence_.get());
}

FenceCollection::FenceCollection(async_dispatcher_t* dispatcher,
                                 fit::function<void(FenceReference*)>&& fired_cb)
    : dispatcher_(dispatcher), fired_cb_(std::move(fired_cb)) {}

void FenceCollection::Clear() {
  // Use a temporary list to prevent double locking when resetting.
  fbl::SinglyLinkedList<fbl::RefPtr<Fence>> fences;
  {
    fbl::AutoLock lock(&mtx_);
    while (!fences_.is_empty()) {
      fences.push_front(fences_.erase(fences_.begin()));
    }
  }
  while (!fences.is_empty()) {
    fences.pop_front()->ClearRef();
  }
}

zx_status_t FenceCollection::ImportEvent(zx::event event, EventId id) {
  fbl::AutoLock lock(&mtx_);
  auto fence = fences_.find(id);
  // Create and ref a new fence.
  if (!fence.IsValid()) {
    // TODO(stevensd): it would be good for this not to be able to fail due to allocation failures
    fbl::AllocChecker ac;
    auto new_fence = fbl::AdoptRef(new (&ac) Fence(this, dispatcher_, id, std::move(event)));
    if (ac.check() && new_fence->CreateRef()) {
      fences_.insert_or_find(std::move(new_fence));
    } else {
      zxlogf(ERROR, "Failed to allocate fence ref for event#%ld", id.value());
      return ZX_ERR_NO_MEMORY;
    }
    return ZX_OK;
  }

  // Ref an existing fence
  if (fence->event() != event.get()) {
    zxlogf(ERROR, "Cannot reuse event#%ld for zx::event %u", id.value(), event.get());
    return ZX_ERR_INVALID_ARGS;
  } else if (!fence->CreateRef()) {
    zxlogf(ERROR, "Failed to allocate fence ref for event#%ld", id.value());
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

void FenceCollection::ReleaseEvent(EventId id) {
  // Hold a ref to prevent double locking if this destroys the fence.
  auto fence_ref = GetFence(id);
  if (fence_ref) {
    fbl::AutoLock lock(&mtx_);
    fences_.find(id)->ClearRef();
  }
}

fbl::RefPtr<FenceReference> FenceCollection::GetFence(EventId id) {
  if (id == kInvalidEventId) {
    return nullptr;
  }
  fbl::AutoLock l(&mtx_);
  auto fence = fences_.find(id);
  return fence.IsValid() ? fence->GetReference() : nullptr;
}

void FenceCollection::OnFenceFired(FenceReference* fence) { fired_cb_(fence); }

void FenceCollection::OnRefForFenceDead(Fence* fence) {
  fbl::AutoLock lock(&mtx_);
  if (fence->OnRefDead()) {
    fences_.erase(fence->id);
  }
}

}  // namespace display
