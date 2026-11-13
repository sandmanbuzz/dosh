; IRQ.COM - sleep in HLT until the clock interrupts us. The program installs
; its own INT 08h handler with AH=25h, enables interrupts and halts. Each
; tick the processor delivers INT 08h through the IVT, the handler counts it
; and irets, and HLT returns. Exits 0 once three ticks have been seen. Proves
; that HLT wakes on ticks (no spin) and that the processor delivers INT 08h
; through the vector table, with the handler preserving the interrupted code.
        org 0x100
        mov ax, 0x2508          ; set vector 08h (clock tick)
        mov dx, tick            ; DS=CS, so the offset is enough
        int 0x21

        sti
idle:   hlt                     ; sleep until the next interrupt
        cmp word [ticks], 3
        jb idle

        cli
        mov dx, msg
        mov ah, 0x09            ; print the message
        int 0x21
        mov ax, 0x4C00
        int 0x21

; The clock handler. Runs with interrupts disabled by the processor; the
; flags it changes are restored by iret. It touches only ticks.
tick:   inc word [cs:ticks]
        iret

ticks:  dw 0
msg:    db 'IRQ OK', 13, 10, '$'
