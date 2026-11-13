; UPPER.COM - a filter: read lines from standard input, write them back in
; upper case, stop at end of file.
        org 0x100
next:   mov ah, 0x3F
        xor bx, bx              ; stdin
        mov cx, 128
        mov dx, buf
        int 0x21
        jc fail
        test ax, ax
        jz done                 ; end of file
        mov cx, ax
        mov bp, ax              ; keep the count; AL is about to be reused
        mov si, buf
.up:    mov al, [si]
        cmp al, 'a'
        jb .keep
        cmp al, 'z'
        ja .keep
        sub byte [si], 0x20
.keep:  inc si
        loop .up
        mov cx, bp
        mov ah, 0x40
        mov bx, 1               ; stdout
        mov dx, buf
        int 0x21
        jmp next
done:   mov ax, 0x4C00
        int 0x21
fail:   mov ax, 0x4C01
        int 0x21
buf:
