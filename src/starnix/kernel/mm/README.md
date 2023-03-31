# Starnix Memory Manager

The memory manager provides Linux memory manipulation operations and maintains the Linux address space in conjunction with Zircon.

## Address space

The address space exposed to Linux programs is contained within one Zircon virtual memory address
range (VMAR) object. This object covers either half or all of the userspace exposed range, depending
on which execution model is in use. Zircon decides where allocations are located within this VMAR
subject to constraints specified by Starnix. This way Zircon's ASLR policies apply to allocations
made by Linux programs as well. Starnix additionally maintains a data structure containing
information about all Linux-exposed allocations and the memory ranges they cover.

## Linux and Zircon models

The Zircon memory model is based on objects and the Linux model is based on address ranges. In
Zircon, memory is allocated by creating and resizing VMOs and memory mappings are changed through
operations on VMARs. In Linux, operations on address ranges such as `mmap()`/`munmap()`/`mremap()`
both allocate/deallocate memory and modify memory mappings. The memory manager bridges between these
models by maintaining a map of Linux address ranges to backing Zircon objects. Linux memory
operations like `mmap()` update both Starnix's model and Zircon's address space mapping. Memory
allocations from Linux can be backed by VMOs created by the memory manager or by VMOs supplied by an
external service such as a Fuchsia filesystem server.

## Unmap and remap

Linux supports unmapping and remapping ranges of memory that may overlap with existing mappings. To
support this, the memory manager must translate these range operations into operations on specific
objects. In some cases this means creating additional objects to represent a mapping of parts of an
allocation. Consider the scenario where a Linux program creates an anonymous private mapping 3 pages
long. The memory manager will allocate a VMO to back this memory and associate it with the mapped
range:

```
0x1234...0000, 0x1234...3000 -> Mapped memory backed by VMO A
```

Then the Linux program unmaps the range 0x1234...1000 -> 0x1234...2000. To handle this, the memory
manager first creates a snapshot child covering the last page of VMO A to use for the top part of
the mapping and then resizes VMO A down to cover the bottom part of the mapping:

```
0x1234...0000, 0x1234...1000 -> Mapped memory backed by VMO A (resized)
0x1234...1000, 0x1234...2000 -> Unmapped
0x1234...2000, 0x1234...3000 -> Mapped memory backed by child VMO
```

## User and kernel mode access

### User mode

Linux programs running in user mode access memory directly through the memory mappings established
by Zircon. Access faults are forwarded from Zircon to the Starnix executor. Some faults, such as
page-not-present page faults, are forward to the memory manager to handle growth of `MAP_GROWSDOWN`
segments (generally used for stack memory).

### Kernel mode

Access from Starnix's "kernel mode" to user memory is handled by first examining the memory manager
map to identify the VMO(s) backing the range of interest and then issuing zx_vmo_{read,write} calls
to interact with these objects.

## Future work

TODO
