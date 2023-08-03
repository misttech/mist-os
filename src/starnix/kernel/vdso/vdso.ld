SECTIONS {
  /* Points the variable vvar (accessible to the vDSO)
     to be one page (4096) above the vDSO */
  vvar = . - 0x1000;
  . = SIZEOF_HEADERS;
  .rodata : {
    *(.rodata*)
   } :load
  .text : { *(.text*) } :load
  .note : { *(.note.gnu.build-id) } :load :note
  .hash : { *(.hash) } :load
  .dynamic : { *(.dynamic) } :load :dynamic
  .gnu.hash : { *(.gnu.hash) } :load
  .dynsym : { *(.dynsym) }
  .dynstr : { *(.dynstr) }
  .got : { *(.got.plt) *(.got) }

  /DISCARD/ : {
    *(.eh_frame)
  }

}

PHDRS {
  /* The LOAD section must include at least FILEHDR so that it covers the start of the file.
     glibc's vDSO loader assumes that the LOAD section has an offset of 0. */
  load PT_LOAD FILEHDR PHDRS FLAGS(5);  /* PF_R|PF_X */
  dynamic PT_DYNAMIC FLAGS(4);          /* PF_R */
  note PT_NOTE FLAGS(4);                /* PF_R */
}
