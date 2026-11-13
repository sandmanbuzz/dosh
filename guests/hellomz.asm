; HELLOMZ.EXE - a multi-segment MZ executable. It only works if the loader
; applied its relocations: one for the data segment, one for a far call.
; Returns 7.
%include "mz.inc"
        mz_header code, start, 0x20
        mz_reloc code, fix_data
        mz_reloc code, fix_far
        mz_image

code:
start:  db 0xB8                 ; mov ax, seg data
fix_data:
        dw SEG_OF(data)
        mov ds, ax
        mov dx, msg - data
        mov ah, 0x09
        int 0x21
        db 0x9A                 ; call far other:shout
        dw shout - other
fix_far:
        dw SEG_OF(other)
        mov ax, 0x4C07
        int 0x21

        align 16
other:
shout:  mov dx, msg2 - data
        mov ah, 0x09
        int 0x21
        retf

        align 16
data:
msg:    db 'Hello from an MZ executable', 13, 10, '$'
msg2:   db 'and from its second code segment', 13, 10, '$'
        mz_end
