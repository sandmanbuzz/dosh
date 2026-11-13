; TRIPLE.COM - destroy the interrupt table, then fault: a triple fault,
; which on a PC resets the machine.
        org 0x100
        lidt [empty]
        int3
empty:  dw 0
        dd 0
