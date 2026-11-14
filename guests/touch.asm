; TOUCH.COM - create the file named on the command line, empty.
; Returns 0, or the DOS error code. Proves a name reaches the volume as DOS
; would canonicalise it, and that the change is what the supervisor stores.
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
        mov ah, 0x3C
        xor cx, cx
        int 0x21
        jc .fail
        mov ax, 0x4C00
        int 0x21
.fail:  mov ah, 0x4C
        int 0x21
