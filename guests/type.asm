; TYPE.COM - copy the file named on the command line to standard output.
; Returns 0, or the DOS error code if the file cannot be opened.
        org 0x100
        mov si, 0x81
.skip:  lodsb
        cmp al, ' '
        je .skip
        dec si
        mov dx, si              ; start of the name
.scan:  lodsb
        cmp al, ' '
        ja .scan
        mov byte [si - 1], 0    ; make it ASCIIZ
        mov ax, 0x3D00          ; open for reading
        int 0x21
        jc .fail
        mov bx, ax
.read:  mov ah, 0x3F
        mov cx, 512
        mov dx, buf
        int 0x21
        jc .fail
        test ax, ax
        jz .done
        mov cx, ax
        push bx
        mov bx, 1
        mov ah, 0x40
        int 0x21
        pop bx
        jmp .read
.done:  mov ah, 0x3E
        int 0x21
        mov ax, 0x4C00
        int 0x21
.fail:  push ax
        mov dx, sorry
        mov ah, 0x09
        int 0x21
        pop ax
        mov ah, 0x4C
        int 0x21
sorry:  db 'File not found', 13, 10, '$'
buf:
