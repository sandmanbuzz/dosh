; BADCALL.COM - ask DOS for a service outside the compatibility contract.
        org 0x100
        mov ax, 0xFF42
        int 0x21
        mov ax, 0x4C00
        int 0x21
