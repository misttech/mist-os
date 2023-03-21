// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.radar/cpp/wire.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/stdcompat/span.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <stdint.h>

#include <optional>

#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "src/lib/vmo_store/vmo_store.h"

namespace radar {

// Thread-safe utility class for keeping track of registered VMOs, their VMARs, and lock states.
class VmoManager {
 public:
  struct RegisteredVmo {
    uint32_t vmo_id;
    cpp20::span<uint8_t> vmo_data;
  };

  explicit VmoManager(size_t minimum_vmo_size)
      : minimum_vmo_size_(minimum_vmo_size),
        registered_vmos_(vmo_store::Options{
            .map = {{
                .vm_option = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                .vmar = nullptr,
            }},
            .pin = {},
        }) {}

  ~VmoManager() {
    // These must be manually cleared before destruction to avoid triggering an assert.
    locked_vmos_.clear();
    unlocked_vmos_.clear();
  }

  // Get the next unlocked VMO, or an empty std::optional if no VMOs are unlocked.
  std::optional<RegisteredVmo> GetUnlockedVmo();

  // Unlock the VMO corresponding to vmo_id.
  void UnlockVmo(uint32_t vmo_id);

  // Register the given VMOs.
  fuchsia_hardware_radar::wire::StatusCode RegisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                                        fidl::VectorView<zx::vmo> vmos);

  // Unregister and return the VMOs corresponding to vmo_ids.
  fuchsia_hardware_radar::wire::StatusCode UnregisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                                          fidl::VectorView<zx::vmo> out_vmos);

 private:
  static constexpr fbl::NodeOptions kNodeOptions = fbl::NodeOptions::AllowMove;

  struct VmoMeta : public fbl::DoublyLinkedListable<VmoMeta*, kNodeOptions> {
    VmoMeta() : fbl::DoublyLinkedListable<VmoMeta*, kNodeOptions>() {}
    uint32_t vmo_id;
    cpp20::span<uint8_t> vmo_data;
  };

  using VmoStore = vmo_store::VmoStore<vmo_store::HashTableStorage<uint32_t, VmoMeta>>;

  const size_t minimum_vmo_size_;

  fbl::Mutex lock_;
  // Doubly linked lists to keep track of which VMOs are locked and which are unlocked. Entries
  // (VmoMeta*) are owned by registered_vmos_.
  fbl::DoublyLinkedList<VmoMeta*> locked_vmos_ TA_GUARDED(lock_);
  fbl::DoublyLinkedList<VmoMeta*> unlocked_vmos_ TA_GUARDED(lock_);
  VmoStore registered_vmos_ TA_GUARDED(lock_);
};

}  // namespace radar
