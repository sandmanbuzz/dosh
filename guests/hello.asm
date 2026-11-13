; HELLO.COM - print a string, exit 0.
        org 0x100
        mov ah, 0x09
        mov dx, msg
        int 0x21
        mov ax, 0x4C00
        int 0x21
msg:    db 'Hello from DOS!', 13, 10, '$'
