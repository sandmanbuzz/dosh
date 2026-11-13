; DATE.COM - print the DOS date and time as "YYYY-MM-DD W HH:MM:SS".
        org 0x100
        mov ah, 0x2A
        int 0x21                ; CX year, DH month, DL day, AL weekday
        push ax
        push dx
        mov ax, cx
        call dec4
        mov al, '-'
        call put
        pop dx
        push dx
        mov al, dh
        call dec2
        mov al, '-'
        call put
        pop dx
        mov al, dl
        call dec2
        mov al, ' '
        call put
        pop ax
        add al, '0'
        call put
        mov al, ' '
        call put
        mov ah, 0x2C
        int 0x21                ; CH hour, CL minute, DH second, DL hundredths
        push dx
        push cx
        mov al, ch
        call dec2
        mov al, ':'
        call put
        pop cx
        mov al, cl
        call dec2
        mov al, ':'
        call put
        pop dx
        mov al, dh
        call dec2
        mov al, 13
        call put
        mov al, 10
        call put
        mov ax, 0x4C00
        int 0x21

dec4:   mov bx, 100             ; ax = 0..9999
        xor dx, dx
        div bx
        push dx
        call dec2
        pop ax
dec2:   aam                     ; al = 0..99
        xchg al, ah
        add al, '0'
        call put
        mov al, ah
        add al, '0'
put:    push ax
        push dx
        mov dl, al
        mov ah, 2
        int 0x21
        pop dx
        pop ax
        ret
