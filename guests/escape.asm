; ESCAPE.COM - try to leave real mode. A capsule must stop this.
        org 0x100
        mov eax, cr0
        or al, 1
        mov cr0, eax
        mov ax, 0x4C00
        int 0x21
