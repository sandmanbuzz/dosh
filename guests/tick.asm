; TICK.COM - read the BIOS tick count until it has advanced by four ticks
; (about 220 ms at 18.2 Hz), then print TICK OK and exit 0. Proves that
; INT 1Ah AH=00h returns a live 32-bit count (CX:DX) that the guest can
; compare across calls, and that the supervisor keeps delivering timer ticks
; to a guest that is only making service calls.
        org 0x100
        mov ah, 0x00
        int 0x1A                ; CX:DX = tick count at start
        mov [start_hi], cx
        mov [start_lo], dx
poll:
        mov ah, 0x00
        int 0x1A                ; CX:DX = tick count now
        sub dx, [start_lo]      ; 32-bit delta = now - start
        sbb cx, [start_hi]
        jnc no_midnight         ; no borrow: the count did not wrap
        add dx, 0x00B0          ; it wrapped at midnight: add one day,
        adc cx, 0x0018          ; 0x1800B0 ticks, back to the delta
no_midnight:
        test cx, cx
        jnz advanced            ; delta >= 65536 ticks: certainly >= 4
        cmp dx, 4
        jb poll                  ; fewer than four ticks so far: ask again
advanced:
        mov ah, 0x09
        mov dx, msg
        int 0x21
        mov ax, 0x4C00
        int 0x21
start_hi: dw 0
start_lo: dw 0
msg:    db 'TICK OK', 13, 10, '$'
