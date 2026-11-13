; RET.COM - terminate the oldest way: a near return to the INT 20h at PSP:0.
; Output goes through character output, BIOS teletype and DOS fast console.
        org 0x100
        mov si, msg
.next:  lodsb
        test al, al
        jz .bios
        mov dl, al
        mov ah, 0x02
        int 0x21
        jmp .next
.bios:  mov ax, 0x0E00 + '!'
        int 0x10
        mov al, 13
        int 0x29
        mov al, 10
        int 0x29
        ret
msg:    db 'returned', 0
