; DIVZERO.COM - raise a processor exception the program does not handle.
        org 0x100
        xor bx, bx
        div bx
        mov ax, 0x4C00
        int 0x21
