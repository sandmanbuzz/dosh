; FILEIO.COM - create a file, write it, seek within it, read it back.
; Prints what it read; returns 0, or the number of the step that failed.
        org 0x100
        mov bp, 1
        mov ah, 0x3C            ; create
        xor cx, cx
        mov dx, name
        int 0x21
        jc fail
        mov bx, ax

        inc bp
        mov ah, 0x40            ; write
        mov cx, text_len
        mov dx, text
        int 0x21
        jc fail
        cmp ax, text_len
        jne fail

        inc bp
        mov ax, 0x4200          ; seek to "world"
        xor cx, cx
        mov dx, 7
        int 0x21
        jc fail

        inc bp
        mov ah, 0x40            ; overwrite in place
        mov cx, 5
        mov dx, patch
        int 0x21
        jc fail

        inc bp
        mov ah, 0x3E            ; close
        int 0x21
        jc fail

        inc bp
        mov ax, 0x3D00          ; reopen read-only
        mov dx, name
        int 0x21
        jc fail
        mov bx, ax

        inc bp
        mov ah, 0x3F
        mov cx, 100
        mov dx, buf
        int 0x21
        jc fail
        cmp ax, text_len
        jne fail

        mov cx, ax
        mov ah, 0x40            ; show it
        mov bx, 1
        mov dx, buf
        int 0x21
        mov ax, 0x4C00
        int 0x21
fail:   mov ax, bp
        mov ah, 0x4C
        int 0x21

name:   db 'C:\OUT.TXT', 0
text:   db 'Hello, world of files', 13, 10
text_len equ $ - text
patch:  db 'WORLD'
buf:
