; HLTCLI.COM - HLT with interrupts disabled. Nothing can wake it, so the
; capsule must fault with a reason rather than sleep forever.
        org 0x100
        cli
        hlt
        mov ax, 0x4C00
        int 0x21
