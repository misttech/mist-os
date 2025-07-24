// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "vm/vm_object.h"

#include <align.h>
#include <assert.h>
#include <inttypes.h>
#include <lib/console.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/algorithm.h>
#include <ktl/utility.h>
#include <vm/physmap.h>
#include <vm/vm.h>
#include <vm/vm_address_region.h>
#include <vm/vm_object_paged.h>

#include "vm_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE VM_GLOBAL_TRACE(0)

fbl::WAVLTreeNodeState<VmMapping*>& VmObject::MappingTreeTraits::node_state(VmMapping& mapping) {
  return mapping.vmo_mapping_node_;
}

VmObject::GlobalList VmObject::all_vmos_ = {};

VmObject::VmObject(VMOType type) : type_(type) { LTRACEF("%p\n", this); }

VmObject::~VmObject() {
  canary_.Assert();
  LTRACEF("%p\n", this);

  DEBUG_ASSERT(!InGlobalList());

  DEBUG_ASSERT(mapping_list_.is_empty());
  DEBUG_ASSERT(children_list_.is_empty());
}

void VmObject::AddToGlobalList() {
  Guard<CriticalMutex> guard{AllVmosLock::Get()};
  all_vmos_.push_back(this);
}

void VmObject::RemoveFromGlobalList() {
  Guard<CriticalMutex> guard{AllVmosLock::Get()};
  DEBUG_ASSERT(InGlobalList());
  all_vmos_.erase(*this);
}

void VmObject::get_name(char* out_name, size_t len) const {
  canary_.Assert();
  name_.get(len, out_name);
}

zx_status_t VmObject::set_name(const char* name, size_t len) {
  canary_.Assert();
  return name_.set(name, len);
}

void VmObject::set_user_id(uint64_t user_id) {
  canary_.Assert();
  DEBUG_ASSERT(user_id_ == 0);
  user_id_ = user_id;
}

uint64_t VmObject::user_id() const {
  canary_.Assert();
  return user_id_;
}

void VmObject::AddMappingLocked(VmMapping* r) {
  canary_.Assert();
  mapping_list_.insert(r);
  mapping_list_len_++;
}

void VmObject::RemoveMappingLocked(VmMapping* r) {
  canary_.Assert();
  mapping_list_.erase(*r);
  DEBUG_ASSERT(mapping_list_len_ > 0);
  mapping_list_len_--;
}

uint32_t VmObject::num_mappings() const {
  canary_.Assert();
  Guard<CriticalMutex> guard{lock()};
  return num_mappings_locked();
}

bool VmObject::IsMappedByUser() const {
  canary_.Assert();
  Guard<CriticalMutex> guard{lock()};
  return ktl::any_of(mapping_list_.cbegin(), mapping_list_.cend(),
                     [](const VmMapping& m) -> bool { return m.aspace()->is_user(); });
}

uint32_t VmObject::share_count() const {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};
  if (mapping_list_len_ < 2) {
    return 1;
  }

  // Find the number of unique VmAspaces that we're mapped into.
  // Use this buffer to hold VmAspace pointers.
  static constexpr int kAspaceBuckets = 64;
  uintptr_t aspaces[kAspaceBuckets];
  unsigned int num_mappings = 0;  // Number of mappings we've visited
  unsigned int num_aspaces = 0;   // Unique aspaces we've seen
  for (const auto& m : mapping_list_) {
    uintptr_t as = reinterpret_cast<uintptr_t>(m.aspace().get());
    // Simple O(n^2) should be fine.
    for (unsigned int i = 0; i < num_aspaces; i++) {
      if (aspaces[i] == as) {
        goto found;
      }
    }
    if (num_aspaces < kAspaceBuckets) {
      aspaces[num_aspaces++] = as;
    } else {
      // Maxed out the buffer. Estimate the remaining number of aspaces.
      num_aspaces +=
          // The number of mappings we haven't visited yet
          (mapping_list_len_ - num_mappings)
          // Scaled down by the ratio of unique aspaces we've seen so far.
          * num_aspaces / num_mappings;
      break;
    }
  found:
    num_mappings++;
  }
  DEBUG_ASSERT_MSG(num_aspaces <= mapping_list_len_,
                   "num_aspaces %u should be <= mapping_list_len_ %" PRIu32, num_aspaces,
                   mapping_list_len_);

  // TODO: Cache this value as long as the set of mappings doesn't change.
  // Or calculate it when adding/removing a new mapping under an aspace
  // not in the list.
  return num_aspaces;
}

void VmObject::SetChildObserver(VmObjectChildObserver* child_observer) {
  Guard<Mutex> guard{ChildObserverLock::Get()};
  child_observer_ = child_observer;
}

bool VmObject::AddChildLocked(VmObject* child) {
  canary_.Assert();
  children_list_.push_front(child);
  children_list_len_++;

  return children_list_len_ == 1;
}

bool VmObject::AddChild(VmObject* child) {
  Guard<CriticalMutex> guard{ChildListLock::Get()};
  return AddChildLocked(child);
}

void VmObject::DropChildLocked(VmObject* c) {
  canary_.Assert();
  DEBUG_ASSERT(children_list_len_ > 0);
  children_list_.erase(*c);
  --children_list_len_;
}

void VmObject::RemoveChild(VmObject* o, Guard<CriticalMutex>::Adoptable adopt) {
  canary_.Assert();

  // The observer may call back into this object so we must release the shared lock to prevent any
  // self-deadlock. We explicitly release the lock prior to acquiring the ChildObserverLock as
  // otherwise we have lock ordering issue, since we already allow the shared lock to be acquired
  // whilst holding the ChildObserverLock.
  {
    Guard<CriticalMutex> guard{AdoptLock, ChildListLock::Get(), ktl::move(adopt)};
    AssertHeld(*ChildListLock::Get());
    DropChildLocked(o);

    if (children_list_len_ != 0) {
      return;
    }
  }

  {
    Guard<Mutex> observer_guard{ChildObserverLock::Get()};

    // Signal the dispatcher that there are no more child VMOS
    if (child_observer_ != nullptr) {
      child_observer_->OnZeroChild();
    }
  }
}

uint32_t VmObject::num_children() const {
  canary_.Assert();
  Guard<CriticalMutex> guard{ChildListLock::Get()};
  return children_list_len_;
}

// static
void VmObject::CacheOpPhys(paddr_t pa, uint64_t len, CacheOpType type,
                           ArchVmICacheConsistencyManager& cm) {
  DEBUG_ASSERT(is_physmap_phys_addr(pa));
  DEBUG_ASSERT(len > 0);

  const vaddr_t va = reinterpret_cast<vaddr_t>(paddr_to_physmap(pa));

  switch (type) {
    case CacheOpType::Invalidate:
      arch_invalidate_cache_range(va, len);
      break;
    case CacheOpType::Clean:
      arch_clean_cache_range(va, len);
      break;
    case CacheOpType::CleanInvalidate:
      arch_clean_invalidate_cache_range(va, len);
      break;
    case CacheOpType::Sync:
      cm.SyncAddr(va, len);
      break;
  }
}

zx_status_t VmObject::GetPageBlocking(uint64_t offset, uint pf_flags, list_node* alloc_list,
                                      vm_page_t** page, paddr_t* pa) {
  zx_status_t status = ZX_OK;
  // TOOD(https://fxbug.dev/42175933): Enforce no locks held as this might wait whilst holding a
  // lock.
  __UNINITIALIZED MultiPageRequest page_request;
  do {
    status = GetPage(offset, pf_flags, alloc_list, &page_request, page, pa);
    if (status == ZX_ERR_SHOULD_WAIT) {
      zx_status_t st = page_request.Wait();
      if (st != ZX_OK) {
        return st;
      }
    }
  } while (status == ZX_ERR_SHOULD_WAIT);

  return status;
}

void VmObject::RangeChangeUpdateMappingsLocked(uint64_t offset, uint64_t len, RangeChangeOp op) {
  canary_.Assert();
  DEBUG_ASSERT(len != 0);
  DEBUG_ASSERT(IS_PAGE_ROUNDED(offset));
  DEBUG_ASSERT(IS_PAGE_ROUNDED(len));

  const uint64_t last_offset = offset + (len - 1);

  // We are going to end up visited nodes in (key) order, and to achieve this walk without a stack
  // we record the key of the nodes as we visit them. This has no effect beyond allowing us to
  // encode a tree walk with constant storage.
  MappingTreeTraits::Key largest_visited = MappingTreeTraits::Key::Min();
  // Begin our search at the root of the tree.
  MappingTree::iterator node = mapping_list_.root();
  using Observer = VmMappingSubtreeState::Observer<VmMapping>;
  while (node) {
    if (MappingTree::iterator left = node.left(); left) {
      // If the left node contains any offsets below the start of our search range, then we need
      // to walk into it. As we do not know the min offset in the left tree we could walk the
      // entire left side of the tree redundantly, but that is just O(log n) nodes which is fine.
      // The largest_visited is compared against to make sure we did not already visit this
      // node/subtree.
      if (Observer::MaxLastOffset(left) >= offset &&
          KeyTraits::LessThan(largest_visited, KeyTraits::GetKey(*left))) {
        node = left;
        continue;
      }
    }

    // Check if this node has been visited yet. This avoids a second visit as we walk back up the
    // tree.
    if (KeyTraits::LessThan(largest_visited, KeyTraits::GetKey(*node))) {
      // Node might be a viable candidate, perform the range update. The mapping will itself check
      // for the precise intersection, if any, first and so it would be duplicate work to precisely
      // check for overlap here.
      VmMapping& m = *node;
      m.assert_object_lock();
      if (op == RangeChangeOp::Unmap) {
        m.AspaceUnmapLockedObject(offset, len, VmMapping::UnmapOptions::kNone);
      } else if (op == RangeChangeOp::UnmapZeroPage) {
        m.AspaceUnmapLockedObject(offset, len, VmMapping::UnmapOptions::OnlyHasZeroPages);
      } else if (op == RangeChangeOp::UnmapAndHarvest) {
        m.AspaceUnmapLockedObject(offset, len, VmMapping::UnmapOptions::Harvest);
      } else if (op == RangeChangeOp::RemoveWrite) {
        m.AspaceRemoveWriteLockedObject(offset, len);
      } else if (op == RangeChangeOp::DebugUnpin) {
        m.AspaceDebugUnpinLockedObject(offset, len);
      } else {
        panic("Unknown RangeChangeOp %d\n", static_cast<int>(op));
      }
      // Record the visit.
      largest_visited = KeyTraits::GetKey(*node);
    }

    if (MappingTree::iterator right = node.right(); right) {
      // By WAVL tree invariant we know that every node in the right subtree has a greater (or
      // equal) offset to the offset in this node. If the first offset of this node is already
      // beyond the range we are updating then, since every node to the right has an even greater
      // offset, that the right tree does not need visiting. Otherwise there might be an
      // overlapping node, and so we search into it, as long as the max offset of the tree is
      // within range.
      // Similar to the left node check, the largest_visisted is just avoiding walking into the
      // node twice.
      if (Observer::FirstOffset(node) <= last_offset && Observer::MaxLastOffset(right) >= offset &&
          KeyTraits::LessThan(largest_visited, KeyTraits::GetKey(*right))) {
        node = right;
        continue;
      }
    }
    // Attempted to visit all sub-trees, return to the parent.
    node = node.parent();
  }
}

// TODO(https://fxbug.dev/408878701): add option to dump by koid.
static int cmd_vm_object(int argc, const cmd_args* argv, uint32_t flags) {
  auto usage_msg = [argv]() {
    printf("usage:\n");
    printf("%s dump <address>\n", argv[0].str);
    printf("%s dump_pages <address>\n", argv[0].str);
    return ZX_ERR_INTERNAL;
  };

  if (argc < 2) {
    printf("not enough arguments\n");
    return usage_msg();
  }

  if (!strcmp(argv[1].str, "dump")) {
    if (argc < 3) {
      printf("not enough arguments\n");
      return usage_msg();
    }

    VmObject* o = reinterpret_cast<VmObject*>(argv[2].u);

    if (o == NULL) {
      printf("invalid address\n");
      return usage_msg();
    }

    o->Dump(0, false);
  } else if (!strcmp(argv[1].str, "dump_pages")) {
    if (argc < 3) {
      printf("not enough arguments\n");
      return usage_msg();
    }

    VmObject* o = reinterpret_cast<VmObject*>(argv[2].u);

    if (o == NULL) {
      printf("invalid address\n");
      return usage_msg();
    }

    o->Dump(0, true);
  } else {
    printf("unknown command\n");
    return usage_msg();
  }

  return ZX_OK;
}

STATIC_COMMAND_START
STATIC_COMMAND("vm_object", "vm object debug commands", &cmd_vm_object)
STATIC_COMMAND_END(vm_object)
