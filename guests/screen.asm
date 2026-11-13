; SCREEN.COM - the text display and the keyboard BIOS, end to end.
; Draws with INT 10h, DOS console output and direct stores to B800:0000,
; waits for one key with INT 16h, shows its scan code, and exits with it.
        org 0x100
        mov ah, 0x0F            ; current mode: the program now has a screen
        int 0x10
        mov ax, 0x0003          ; set 80x25 colour text, clearing it
        int 0x10
        mov dx, 0x020A          ; DOS console output goes where the cursor is
        xor bh, bh
        mov ah, 0x02
        int 0x10
        mov dx, hello
        mov ah, 0x09
        int 0x21
        mov ax, 0xB800          ; direct stores into the text buffer
        mov es, ax
        mov di, 5 * 160
        mov si, direct
        mov ah, 0x1F            ; white on blue
.copy:  lodsb
        test al, al
        jz .copied
        stosw
        jmp .copy
.copied:
        mov word [es:6 * 160], 0x4EC9   ; a double-line corner, yellow on red
        xor ah, ah              ; wait for a key
        int 0x16
        mov [key], ah
        mov dx, 0x0800          ; row 8
        xor bh, bh
        mov ah, 0x02
        int 0x10
        mov dx, scan
        mov ah, 0x09
        int 0x21
        mov al, [key]
        shr al, 4
        call digit
        mov al, [key]
        and al, 15
        call digit
        mov ah, 0x4C
        mov al, [key]
        int 0x21
digit:  add al, '0'
        cmp al, '9'
        jbe .out
        add al, 7
.out:   mov dl, al
        mov ah, 0x02
        int 0x21
        ret
hello:  db 'Hello, screen$'
direct: db 'Direct store', 0
scan:   db 'scan=$'
key:    db 0
