; COUNTER.COM - the smallest program with memory. Reads the byte in
; COUNT.DAT (absent means 0), adds one, writes it back, prints the new value
; as a decimal number, and exits with it. Run on a machine whose drive C:
; persists, it counts the times the machine has run it.
        org 0x100
        xor bx, bx              ; the count, if there is a file
        mov ax, 0x3D00          ; open read-only
        mov dx, name
        int 0x21
        jc .none
        mov si, ax
        mov bx, si
        mov ah, 0x3F            ; read one byte
        mov cx, 1
        mov dx, count
        int 0x21
        mov ah, 0x3E
        mov bx, si
        int 0x21
.none:  inc byte [count]        ; stays 0+1 if the read found nothing
        mov ah, 0x3C            ; create (truncate)
        xor cx, cx
        mov dx, name
        int 0x21
        jc .fail
        mov bx, ax
        mov ah, 0x40
        mov cx, 1
        mov dx, count
        int 0x21
        jc .fail
        mov ah, 0x3E
        int 0x21

        mov al, [count]         ; print as decimal
        xor ah, ah
        mov cl, 100
        div cl
        mov dh, ah              ; remainder
        add al, '0'
        mov dl, al
        mov ah, 0x02
        int 0x21
        mov al, dh
        xor ah, ah
        mov cl, 10
        div cl
        mov dh, ah
        add al, '0'
        mov dl, al
        mov ah, 0x02
        int 0x21
        mov dl, dh
        add dl, '0'
        mov ah, 0x02
        int 0x21
        mov al, [count]
        mov ah, 0x4C
        int 0x21
.fail:  mov ax, 0x4CFF
        int 0x21

name:   db 'COUNT.DAT', 0
count:  db 0
