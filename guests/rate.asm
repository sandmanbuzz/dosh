; RATE.COM - the clock interrupt arrives at the BIOS rate, not at the rate the
; scheduler happens to run this program. The program installs its own INT 08h
; handler, sleeps in HLT between ticks, and measures elapsed time with the BIOS
; tick count (INT 1Ah AH=00h, about 18.2 per second). It stops after 18 ticks
; of elapsed time and then counts how many INT 08h arrived in that interval.
; A count of 16 to 19 means delivery follows elapsed time: a handler run on
; every scheduling quantum or on every guest exit would count far more, and a
; clock that stopped while another capsule ran would count far fewer.
        org 0x100
        mov ax, 0x2508          ; set vector 08h (clock tick)
        mov dx, tick            ; DS=CS, so the offset is enough
        int 0x21

        sti
        mov ah, 0x00
        int 0x1A                ; CX:DX = tick count at start
        mov [start_hi], cx
        mov [start_lo], dx
idle:   hlt                     ; sleep until the next tick
        mov ah, 0x00
        int 0x1A                ; CX:DX = tick count now
        sub dx, [start_lo]      ; 32-bit delta = now - start
        sbb cx, [start_hi]
        jnc no_midnight         ; no borrow: the count did not wrap
        add dx, 0x00B0          ; it wrapped at midnight: add one day,
        adc cx, 0x0018          ; 0x1800B0 ticks, back to the delta
no_midnight:
        test cx, cx
        jnz elapsed             ; delta >= 65536 ticks: certainly >= 18
        cmp dx, 18
        jb idle                 ; fewer than 18 ticks so far: sleep again
elapsed:
        cli
        mov ax, [delivered]
        cmp ax, 16
        jb bad
        cmp ax, 19
        ja bad
        mov dx, ok_msg
        jmp show
bad:    mov dx, bad_msg
show:   mov ah, 0x09            ; print the verdict
        int 0x21
        mov ax, 0x4C00
        int 0x21

; The clock handler. The processor clears IF on entry and iret restores the
; flags. It touches only the counter, so the interrupted code is unaffected.
tick:   inc word [cs:delivered]
        iret

delivered: dw 0
start_hi:  dw 0
start_lo:  dw 0
ok_msg:    db 'RATE OK', 13, 10, '$'
bad_msg:   db 'RATE BAD', 13, 10, '$'
