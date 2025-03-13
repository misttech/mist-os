// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_EVENT_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_EVENT_DISPATCHER_H_

#include <sys/types.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <object/dispatcher.h>
#include <object/handle.h>

class EventDispatcher
    : public SoloDispatcher<EventDispatcher, ZX_DEFAULT_EVENT_RIGHTS, ZX_EVENT_SIGNALED> {
 public:
  static zx_status_t Create(uint32_t options, KernelHandle<EventDispatcher>* handle,
                            zx_rights_t* rights);

  ~EventDispatcher();
  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_EVENT; }

 protected:
  explicit EventDispatcher(uint32_t options);
};

class MemoryStallEventDispatcher final : public EventDispatcher,
                                         private StallObserver::EventReceiver {
 public:
  static zx_status_t Create(zx_system_memory_stall_type_t kind, zx_duration_mono_t threshold,
                            zx_duration_mono_t window, KernelHandle<EventDispatcher>* handle,
                            zx_rights_t* rights);

  ~MemoryStallEventDispatcher() final;

 private:
  explicit MemoryStallEventDispatcher(zx_system_memory_stall_type_t kind);

  void OnAboveThreshold() override;
  void OnBelowThreshold() override;

  // The StallObserver that decides if we are currently above or below the
  // threshold and notifies us through the EventReceiver interface. There is a
  // 1:1 correspondence between instances of StallObserver and
  // MemoryStallEventDispatcher.
  //
  // This would ideally be const (once set, the pointer never changes), but it
  // cannot be because the StallObserver is created after our constructor has
  // already been executed.
  ktl::unique_ptr<StallObserver> observer_;

  // The kind of memory stall that the observer_ has been registered to (used by
  // our destructor to unregister it).
  const zx_system_memory_stall_type_t kind_;
};

fbl::RefPtr<EventDispatcher> GetMemPressureEvent(uint32_t kind);

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_EVENT_DISPATCHER_H_
