// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_ABI_STUB_H_
#define LIB_LD_REMOTE_ABI_STUB_H_

#include <lib/elfldltl/symbol.h>

#include "abi.h"
#include "module.h"
#include "remote-load-module.h"

namespace ld {

// In the remote dynamic linking case, the passive ABI is anchored by the "stub
// dynamic linker" (ld-stub.so in the build).  This shared library gets loaded
// as an implicit dependency to stand in for the in-process startup dynamic
// linker that's usually there at runtime.  Aside from the normal metadata
// (TODO(https://fxbug.dev/318890954): it will later have a code segment for
// the TLSDESC callbacks too), it consists of a final ZeroFillSegment or
// DataWithZeroFillSegment that holds space for the passive ABI symbols.
//
// The RemoteAbiStub object collects data about the layout and symbols in the
// stub dynamic linker as decoded into a RemoteLoadModule.  The Init() method
// examines the module and records its layout, but does not refer to the
// argument RemoteLoadModule object thereafter.  The RemoteAbiStub object can
// be reused or trivially copied as long as the same stub dynamic linker ELF
// file (or verbatim copy) is being used.
//
// This object is used by the RemoteAbiHeap to modify a RemoteLoadModule for
// the specific instantiation of the stub dynamic linker for a particular
// remote dynamic linking domain (or zygote thereof).

template <class Elf = elfldltl::Elf<>>
class RemoteAbiStub {
 public:
  using size_type = typename Elf::size_type;
  using Addr = typename Elf::Addr;
  using Sym = typename Elf::Sym;
  using RemoteModule = RemoteLoadModule<Elf>;
  using LocalAbi = abi::Abi<Elf>;

  // Exact size of the data segment.  This includes whatever file data gives it
  // a page-aligned starting vaddr, but the total size is not page-aligned.
  // Space directly after this can be used to lengthen the segment.
  size_type data_size() const { return data_size_; }

  // Offset into the segment where _ld_abi sits.
  size_type abi_offset() const { return abi_offset_; }

  // Offset into the segment where _r_debug sits.
  size_type rdebug_offset() const { return rdebug_offset_; }

  // Init() calculates and records all those values by examining the stub
  // dynamic linker previously decoded.  The module reference is only used to
  // examine and is not saved.  It need not be the same module data structure
  // that's actually prepared and loaded using those values, but it must be one
  // coming from the the same ELF file (or a verbatim copy thereof).
  template <class Diagnostics>
  bool Init(Diagnostics& diag, const RemoteModule& ld_stub) {
    if (ld_stub.load_info().segments().empty()) [[unlikely]] {
      return diag.FormatError("stub ", LocalAbi::kSoname.str(), " has no segments");
    }

    // Find the unrounded vaddr size.  The PT_LOADs are in ascending vaddr
    // order, so the last one will be the highest addressed, while the module's
    // vaddr_start will correspond to the first one's vaddr.
    for (auto it = ld_stub.module().phdrs.rbegin(); it != ld_stub.module().phdrs.rend(); ++it) {
      if (it->type == elfldltl::ElfPhdrType::kLoad) {
        data_size_ = it->vaddr + it->memsz;
        break;
      }
    }
    assert(ld_stub.load_info().VisitSegment(
        [this](const auto& segment) {
          return segment.vaddr() <= data_size_ && segment.vaddr() + segment.memsz() >= data_size_;
        },
        ld_stub.load_info().segments().back()));
    size_type stub_data_vaddr;
    ld_stub.load_info().VisitSegment(
        [&stub_data_vaddr](const auto& segment) -> std::true_type {
          stub_data_vaddr = segment.vaddr();
          return {};
        },
        ld_stub.load_info().segments().back());
    data_size_ -= stub_data_vaddr;

    auto get_offset = [this, &diag, &ld_stub, stub_data_vaddr](  //
                          size_type& offset, const elfldltl::SymbolName& name,
                          size_t size) -> bool {
      const Sym* symbol = name.Lookup(ld_stub.module().symbols);
      if (!symbol) [[unlikely]] {
        return diag.FormatError("stub ", LocalAbi::kSoname.str(), " does not define ", name,
                                " symbol");
      }
      if (symbol->size != size) [[unlikely]] {
        return diag.FormatError("stub ", LocalAbi::kSoname.str(), " ", name, " symbol size ",
                                symbol->size, " != expected ", size);
      }
      if (symbol->value < stub_data_vaddr || symbol->value - stub_data_vaddr > data_size_ - size)
          [[unlikely]] {
        return diag.FormatError("stub ", LocalAbi::kSoname.str(), " ", name,
                                elfldltl::FileAddress{symbol->value}, " can't fit ", size,
                                " bytes in ", data_size_, "-byte data segment",
                                elfldltl::FileAddress{stub_data_vaddr});
      }
      offset = symbol->value - stub_data_vaddr;
      return true;
    };

    constexpr auto no_overlap = [](size_type start1, size_type len1, size_type start2,
                                   size_type len2) -> bool {
      return start1 >= start2 + len2 || start2 >= start1 + len1;
    };

    return get_offset(abi_offset_, abi::kAbiSymbol, sizeof(Abi)) &&
           get_offset(rdebug_offset_, abi::kRDebugSymbol, sizeof(RDebug)) &&
           (no_overlap(abi_offset_, sizeof(Abi), rdebug_offset_, sizeof(RDebug)) ||
            diag.FormatError("stub ", LocalAbi::kSoname.str(), " symbols overlap!"));
  }

 private:
  using Abi = abi::Abi<Elf, elfldltl::RemoteAbiTraits>;
  using RDebug = typename Elf::template RDebug<elfldltl::RemoteAbiTraits>;

  size_type abi_offset_ = 0;
  size_type rdebug_offset_ = 0;
  size_type data_size_ = 0;
};

}  // namespace ld

#endif  // LIB_LD_REMOTE_ABI_STUB_H_
