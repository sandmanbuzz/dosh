; ARGS.COM - print the command tail between brackets; return its length.
        org 0x100
        mov dl, '['
        call putc
        mov si, 0x81
        xor cx, cx
        mov cl, [0x80]
        jcxz .done
.next:  lodsb
        mov dl, al
        call putc
        loop .next
.done:  mov dl, ']'
        call putc
        mov dl, 13
        call putc
        mov dl, 10
        call putc
        mov al, [0x80]
        mov ah, 0x4C
        int 0x21

putc:   mov ah, 0x02
        int 0x21
        ret
