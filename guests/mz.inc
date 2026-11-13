; A hand-written MZ header for flat-binary assembly.
;
;   %include "mz.inc"
;   mz_header entry_seg, entry, stack_paras    ; labels in the image
;   mz_reloc label_seg, label                  ; one per segment fix-up
;   mz_image
;   ... segments, each starting on `align 16` ...
;   mz_end
;
; Segment values in the image are paragraphs from `mz_image`; use SEG_OF(x)
; and list the address of each such word with mz_reloc.

%define SEG_OF(label) (((label) - mz.image) / 16)

%macro mz_header 3
        bits 16
mz.header:
        db 'MZ'
        dw (mz.end - mz.header) % 512           ; bytes in the last page
        dw (mz.end - mz.header + 511) / 512     ; pages
        dw (mz.relocs_end - mz.relocs) / 4
        dw (mz.image - mz.header) / 16          ; header paragraphs
        dw %3                                   ; minimum extra: the stack
        dw %3                                   ; maximum extra
        dw SEG_OF(mz.end)                       ; SS: just past the image
        dw (%3) * 16                            ; SP
        dw 0                                    ; checksum
        dw (%2) - (%1)                          ; IP
        dw SEG_OF(%1)                           ; CS
        dw mz.relocs - mz.header
        dw 0                                    ; overlay
mz.relocs:
%endmacro

%macro mz_reloc 2
        dw (%2) - (%1), SEG_OF(%1)
%endmacro

%macro mz_image 0
mz.relocs_end:
        align 16, db 0
mz.image:
%endmacro

%macro mz_end 0
        align 16, db 0
mz.end:
%endmacro
