; A20.COM - a capsule starts with the A20 line off: addresses past 1 MiB
; wrap around to the start of memory, as on the original PC. Returns 0 if
; FFFF:0510 and 0000:0500 are the same byte.
        org 0x100
        mov ax, 0xFFFF
        mov es, ax
        xor ax, ax
        mov ds, ax
        mov byte [0x0500], 0x5A
        cmp byte [es:0x0510], 0x5A
        jne .differ
        mov byte [es:0x0510], 0xA5
        cmp byte [0x0500], 0xA5
        jne .differ
        mov ax, 0x4C00
        int 0x21
.differ:
        mov ax, 0x4C01
        int 0x21
