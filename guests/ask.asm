; ASK.COM - interactive console: a buffered line, then a single keystroke.
; Returns the keystroke.
        org 0x100
        mov dx, prompt
        mov ah, 0x09
        int 0x21
        mov dx, line
        mov ah, 0x0A            ; buffered input
        int 0x21
        mov dx, hello
        mov ah, 0x09
        int 0x21
        xor bx, bx
        mov bl, [line + 1]
        mov byte [line + 2 + bx], '$'
        mov dx, line + 2
        mov ah, 0x09
        int 0x21
        mov dx, again
        mov ah, 0x09
        int 0x21
        mov ah, 0x08            ; one key, no echo
        int 0x21
        mov ah, 0x4C
        int 0x21
prompt: db 'Name? $'
hello:  db 10, 'Hello, $'
again:  db '!', 13, 10, 'Key? $'
line:   db 16, 0
        times 17 db 0
