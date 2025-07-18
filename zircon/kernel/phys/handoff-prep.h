// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_
#define ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_

#include <lib/fit/function.h>
#include <lib/trivial-allocator/basic-leaky-allocator.h>
#include <lib/trivial-allocator/new.h>
#include <lib/trivial-allocator/page-allocator.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/image.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_single_list.h>
#include <ktl/algorithm.h>
#include <ktl/byte.h>
#include <ktl/concepts.h>
#include <ktl/initializer_list.h>
#include <ktl/span.h>
#include <ktl/tuple.h>
#include <ktl/utility.h>
#include <phys/elf-image.h>
#include <phys/handoff-ptr.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/new.h>
#include <phys/uart.h>
#include <phys/zbitl-allocation.h>
#include <phys/zircon-abi-spec.h>

struct ArchPatchInfo;
struct BootOptions;
class PhysBootTimes;
class ElfImage;
class Log;

class HandoffPrep {
 public:
  explicit HandoffPrep(ElfImage kernel);

  // This is the main structure.
  PhysHandoff* handoff() { return handoff_; }

  // This returns new T(args...) using the temporary handoff allocator and
  // fills in the handoff_ptr to point to it.
  template <typename T, PhysHandoffPtrLifetime Lifetime, typename... Args>
  T* New(PhysHandoffPtr<const T, Lifetime>& handoff_ptr, fbl::AllocChecker& ac, Args&&... args) {
    T* ptr;
    if constexpr (Lifetime == PhysHandoffPtrLifetime::Temporary) {
      ptr = new (temporary_data_allocator_, ac) T(ktl::forward<Args>(args)...);
    } else {
      ptr = new (permanent_data_allocator_, ac) T(ktl::forward<Args>(args)...);
    }
    if (ptr) {
      handoff_ptr.ptr_ = ptr;
    }
    return ptr;
  }

  // Similar but for new T[n] using spans instead of pointers.
  template <typename T, PhysHandoffPtrLifetime Lifetime>
  ktl::span<T> New(PhysHandoffSpan<const T, Lifetime>& handoff_span, fbl::AllocChecker& ac,
                   size_t n) {
    T* ptr;
    if constexpr (Lifetime == PhysHandoffPtrLifetime::Temporary) {
      ptr = new (temporary_data_allocator_, ac) T[n];
    } else {
      ptr = new (permanent_data_allocator_, ac) T[n];
    }
    if (ptr) {
      handoff_span.ptr_.ptr_ = ptr;
      handoff_span.size_ = n;
      return {ptr, n};
    }
    return {};
  }

  template <PhysHandoffPtrLifetime Lifetime>
  ktl::string_view New(PhysHandoffString<Lifetime>& handoff_string, fbl::AllocChecker& ac,
                       ktl::string_view str) {
    ktl::span chars = New(handoff_string, ac, str.size());
    if (chars.empty()) {
      return {};
    }
    ZX_DEBUG_ASSERT(chars.size() == str.size());
    return {chars.data(), str.copy(chars.data(), chars.size())};
  }

  // This does all the main work of preparing for the kernel, and then calls
  // `boot` to transfer control to the kernel entry point with the handoff()
  // pointer as its argument. The `boot` function should do nothing but hand
  // off to the kernel; in particular, state has already been captured from
  // `uart` so no additional printing should be done at this stage.
  [[noreturn]] void DoHandoff(UartDriver& uart, ktl::span<ktl::byte> zbi,
                              const KernelStorage::Bootfs& kernel_package,
                              const ArchPatchInfo& patch_info);

  // Add an additonal, generic VMO to be simply published to userland.  The
  // kernel proper won't ever look at it.
  void PublishExtraVmo(PhysVmo&& vmo);

 private:
  // TODO(https://fxbug.dev/42164859): Populate with prepared elements of C++
  // ABI set-up (e.g., mapped stacks).
  struct ZirconAbi {
    uintptr_t machine_stack_top = 0;
    uintptr_t shadow_call_stack_base = 0;
  };

  // Comprises a list in scratch memory of the pending VM objects so they can
  // be packed into a single array at the end (via NewFromList()).
  template <ktl::derived_from<PhysVmObject> VmObject>
  struct HandoffVmObject : public fbl::SinglyLinkedListable<HandoffVmObject<VmObject>*> {
    static HandoffVmObject* New(VmObject obj) {
      fbl::AllocChecker ac;
      HandoffVmObject* handoff_obj =
          new (gPhysNew<memalloc::Type::kPhysScratch>, ac) HandoffVmObject;
      ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu scratch bytes for hand-off VM object",
                    sizeof(*handoff_obj));
      handoff_obj->object = ktl::move(obj);
      return handoff_obj;
    }

    VmObject object;
  };

  template <typename VmObject>
  using HandoffVmObjectList = fbl::SizedSinglyLinkedList<HandoffVmObject<VmObject>*>;

  using HandoffVmo = HandoffVmObject<PhysVmo>;
  using HandoffVmoList = HandoffVmObjectList<PhysVmo>;

  using HandoffVmar = HandoffVmObject<PhysVmar>;
  using HandoffVmarList = HandoffVmObjectList<PhysVmar>;

  using HandoffMapping = HandoffVmObject<PhysMapping>;
  using HandoffMappingList = HandoffVmObjectList<PhysMapping>;

  // Defined in handoff-prep-vm.cc.
  class VirtualAddressAllocator {
   public:
    enum class Strategy : bool { kDown, kUp };

    // The allocator for temporary hand-off data.
    static VirtualAddressAllocator TemporaryHandoffDataAllocator(const ElfImage& kernel);

    // The allocator for permanent hand-off data.
    static VirtualAddressAllocator PermanentHandoffDataAllocator(const ElfImage& kernel);

    // The allocator for first-class hand-off mappings (i.e., for important,
    // one-off things, likely to be packaged in their own VMARs).
    static VirtualAddressAllocator FirstClassMappingAllocator(const ElfImage& kernel);

    VirtualAddressAllocator(uintptr_t start, Strategy strategy,
                            ktl::optional<uintptr_t> boundary = ktl::nullopt)
        : start_{start}, strategy_{strategy}, boundary_{boundary} {
      if (boundary) {
        switch (strategy) {
          case Strategy::kDown:
            ZX_DEBUG_ASSERT(start >= *boundary);
            break;
          case Strategy::kUp:
            ZX_DEBUG_ASSERT(start <= *boundary);
            break;
        }
      }
    }

    uintptr_t AllocatePages(size_t size);

   private:
    uintptr_t start_;
    Strategy strategy_;
    ktl::optional<uintptr_t> boundary_;
  };

  template <memalloc::Type Type>
  class PhysPages {
   public:
    struct Capability {
      Allocation phys;
      void* virt;
    };

    template <typename... Args>
    explicit PhysPages(Args&&... args) : va_allocator_(ktl::forward<Args>(args)...) {}

    HandoffMappingList&& TakeMappings() { return ktl::move(mappings_); }

    size_t page_size() const { return ZX_PAGE_SIZE; }

    [[nodiscard]] ktl::pair<void*, Capability> Allocate(size_t size) {
      fbl::AllocChecker ac;
      Allocation pages = Allocation::New(ac, Type, size, ZX_PAGE_SIZE);
      if (!ac.check()) {
        return {};
      }

      const char* mapping_name;
      if constexpr (Type == memalloc::Type::kTemporaryPhysHandoff) {
        mapping_name = "temporary hand-off data";
      } else {
        static_assert(Type == memalloc::Type::kPermanentPhysHandoff);
        mapping_name = "permanent hand-off data";
      }

      uintptr_t vaddr = va_allocator_.AllocatePages(size);
      uintptr_t paddr = reinterpret_cast<uintptr_t>(pages.get());
      PhysMapping mapping(mapping_name, PhysMapping::Type::kNormal, vaddr, size, paddr,
                          PhysMapping::Permissions::Rw());
      void* ptr = static_cast<void*>(CreateMapping(mapping).data());
      mappings_.push_front(HandoffMapping::New(ktl::move(mapping)));

      return {ptr, Capability{ktl::move(pages), ptr}};
    }

    void Deallocate(Capability allocation, void* ptr, size_t size) {
      ZX_DEBUG_ASSERT(ptr == allocation.virt);
      ZX_DEBUG_ASSERT(size == allocation.phys.size_bytes());
      // Note: `allocation.phys` will free itself when it goes out of scope
      // on returning.
    }

    void Release(Capability allocation, void* ptr, size_t size) {
      ZX_DEBUG_ASSERT(ptr == allocation.virt);
      ZX_DEBUG_ASSERT(size == allocation.phys.size_bytes());
      ktl::ignore = allocation.phys.release();
    }

    void Seal(Capability, void*, size_t) { ZX_PANIC("Unexpected call to Seal::Capability"); }

   private:
    VirtualAddressAllocator va_allocator_;
    HandoffMappingList mappings_;
  };

  template <memalloc::Type Type>
  using PageAllocationFunction = trivial_allocator::PageAllocator<PhysPages<Type>>;

  template <memalloc::Type Type>
  using Allocator = trivial_allocator::BasicLeakyAllocator<PageAllocationFunction<Type>>;

  using TemporaryDataAllocator = Allocator<memalloc::Type::kTemporaryPhysHandoff>;
  using PermanentDataAllocator = Allocator<memalloc::Type::kPermanentPhysHandoff>;

  // A convenience class for building up a PhysVmar.
  class PhysVmarPrep {
   public:
    constexpr PhysVmarPrep() = default;

    // Creates the provided mapping and publishes it within the associated VMAR
    // being built up.
    MappedMemoryRange PublishMapping(PhysMapping mapping) {
      ZX_DEBUG_ASSERT(vmar_.base <= mapping.vaddr);
      ZX_DEBUG_ASSERT(mapping.vaddr_end() <= vmar_.end());
      ktl::span mapped = CreateMapping(mapping);
      mappings_.push_front(HandoffMapping::New(ktl::move(mapping)));
      return {mapped, mapping.paddr};
    }

    // Publishes the PhysVmar in the hand-off.
    void Publish() && {
      prep_->NewFromList(vmar_.mappings, ktl::move(mappings_));
      prep_->vmars_.push_front(HandoffVmar::New(ktl::move(vmar_)));
      prep_ = nullptr;
    }

   private:
    friend class HandoffPrep;

    explicit PhysVmarPrep(HandoffPrep* prep, ktl::string_view name, uintptr_t base, size_t size)
        : prep_(prep), vmar_{.base = base, .size = size} {
      vmar_.set_name(name);
    }

    HandoffPrep* prep_ = nullptr;
    PhysVmar vmar_;
    HandoffMappingList mappings_;
  };

  struct Debugdata {
    ktl::string_view announce, sink_name, vmo_name;
    size_t size_bytes = 0;
  };

  // Constructs a PhysVmo from the provided information, enforcing that `data`
  // is page-aligned and that page-rounding `content_size` up yields
  // `data.size_bytes()`.
  static PhysVmo MakePhysVmo(ktl::span<const ktl::byte> data, ktl::string_view name,
                             size_t content_size);

  static MappedMemoryRange CreateMapping(const PhysMapping& mapping);

  // Packs a list of pending VM objects into a single hand-off span in sorted
  // order.
  template <typename VmObject>
  ktl::span<VmObject> NewFromList(PhysHandoffTemporarySpan<const VmObject>& span,
                                  HandoffVmObjectList<VmObject> list) {
    fbl::AllocChecker ac;
    ktl::span objects = New(span, ac, list.size());
    ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu * %zu-byte VM object span", list.size(),
                  sizeof(VmObject));
    ZX_DEBUG_ASSERT(objects.size() == list.size());

    for (VmObject& obj : objects) {
      obj = ktl::move(list.pop_front()->object);
    }
    ktl::sort(objects.begin(), objects.end());
    return objects;
  }

  void SaveForMexec(const zbi_header_t& header, ktl::span<const ktl::byte> payload);

  // The arch-specific protocol for a given item.
  // Defined in //zircon/kernel/arch/$cpu/phys/arch-handoff-prep-zbi.cc.
  void ArchSummarizeMiscZbiItem(const zbi_header_t& header, ktl::span<const ktl::byte> payload);

  // Fills in handoff()->boot_options and returns the mutable reference to
  // update its fields later so that `.serial` can be transferred last.
  BootOptions& SetBootOptions(const BootOptions& boot_options);

  // Fetch things to be handed off from other files in the kernel package.
  void UsePackageFiles(KernelStorage::Bootfs kernel_package);
  void SetVersionString(ktl::string_view version);

  // Summarizes the provided data ZBI's miscellaneous simple items for the
  // kernel, filling in corresponding handoff()->item fields.  Certain fields,
  // may be cleaned after consumption for security considerations, such as
  // 'ZBI_TYPE_SECURE_ENTROPY'.
  void SummarizeMiscZbiItems(ktl::span<ktl::byte> zbi);

  // Add physboot's own instrumentation data to the handoff.  After this, the
  // live instrumented physboot code is updating the handoff data directly up
  // through the very last compiled basic block that jumps into the kernel.
  // This calls PublishExtraVmo, so it must come before FinishExtraVmos.
  void SetInstrumentation();

  // Do PublishExtraVmo with a Log buffer, which is consumed.
  void PublishLog(ktl::string_view vmo_name, Log&& log);

  // Constructs a prep object for publishing a PhysVmar.
  PhysVmarPrep PrepareVmarAt(ktl::string_view name, uintptr_t base, size_t size) {
    PhysVmarPrep prep;
    prep.prep_ = this;
    prep.vmar_ = PhysVmar{.base = base, .size = size};
    prep.vmar_.set_name(name);
    return prep;
  }

  // Publishes a PhysVmar with a single mapping covering its extent, returning
  // its mapped virtual address range.
  MappedMemoryRange PublishSingleMappingVmar(PhysMapping mapping);

  // A variation that assumes a first-class mapping and allocates and the
  // virtual addresses itself. The provided address range may be
  // non-page-aligned, in which the virtual mapping of [addr, addr + size) is
  // returned directly rather than with page alignment.
  MappedMemoryRange PublishSingleMappingVmar(ktl::string_view name, PhysMapping::Type type,
                                             uintptr_t addr, size_t size,
                                             PhysMapping::Permissions perms);

  // A specialization for an MMIO range.
  MappedMmioRange PublishSingleMmioMappingVmar(ktl::string_view name, uintptr_t addr, size_t size) {
    return PublishSingleMappingVmar(name, PhysMapping::Type::kMmio, addr, size,
                                    PhysMapping::Permissions::Rw());
  }

  // A specialization for an MMIO range.
  MappedRange<ktl::byte> PublishSingleWritableDataMappingVmar(ktl::string_view name, uintptr_t addr,
                                                              size_t size) {
    return PublishSingleMappingVmar(name, PhysMapping::Type::kNormal, addr, size,
                                    PhysMapping::Permissions::Rw());
  }

  MappedMemoryRange PublishStackVmar(ZirconAbiSpec::Stack stack, memalloc::Type type);

  // This constructs a PhysElfImage from an ELF file in the KernelStorage.
  PhysElfImage MakePhysElfImage(KernelStorage::Bootfs::iterator file, ktl::string_view name);

  // Do final handoff of the VM object lists.  The contents are already in
  // place so this does not invalidate any pointers to the objects (e.g., from
  // PublishExtraVmo).
  void FinishVmObjects();

  // Normalizes and publishes RAM and the allocations of interest to the kernel.
  //
  // This must be the very last set-up routine called within DoHandoff().
  void SetMemory();

  // Constructs and populates the kernel's address space, and returns the
  // mapped realizations of its ABI requirements per abi_spec_.
  ZirconAbi ConstructKernelAddressSpace(const UartDriver& uart);
  void ArchConstructKernelAddressSpace();  // The arch-specific subroutine

  // Finalizes handoff_.arch and performs the final, architecture-specific
  // subroutine of DoHandoff().
  //
  // This call intends to hand off - and thus either explicitly set or
  // explicitly clear to zero - all of the aspects of machine state that
  // constitute the C++ ABI, but to leave the rest of the machine state (like
  // exception handlers) in the ambient phys state until the kernel is on its
  // feet far enough to reset all that stuff for itself.
  //
  // Note that by the time this has been called the UART driver has been taken
  // and no more logging is permitted.
  [[noreturn]] void ArchDoHandoff(ZirconAbi abi, const ArchPatchInfo& patch_info);

  const ElfImage kernel_;
  PhysHandoff* handoff_ = nullptr;
  ZirconAbiSpec abi_spec_{};
  TemporaryDataAllocator temporary_data_allocator_;
  PermanentDataAllocator permanent_data_allocator_;
  VirtualAddressAllocator first_class_mapping_allocator_;
  zbitl::Image<Allocation> mexec_image_;
  HandoffVmarList vmars_;
  HandoffVmoList extra_vmos_;
};

#endif  // ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_
