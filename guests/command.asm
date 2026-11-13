; COMMAND.COM - the DOSH command interpreter: a prompt, a handful of
; built-ins (DIR, TYPE, ECHO, VER, EXIT) and EXEC for everything else, with
; .COM tried before .EXE when the name has no extension.
; Proves a program can be an operating environment: it keeps its own state
; across the children it starts and sends each one its command tail.
        org 0x100
        mov ah, 0x4A            ; keep 64 KiB; the rest is for children
        mov bx, 0x1000
        int 0x21
        mov [pb+4], cs          ; segments of the EXEC parameter block
        mov [pb+8], cs
        mov [pb+12], cs
        mov dx, dta
        mov ah, 0x1A
        int 0x21
        mov dx, banner
        call puts
prompt: push cs
        pop ds
        push cs
        pop es
        mov dx, ps
        call puts
        mov dx, line
        mov ah, 0x0A
        int 0x21
        mov dl, 10              ; the call echoed the Enter as CR; finish the line
        call putc
        mov si, line+2
        call skipsp
        cmp byte [si], 13
        je prompt
        ; the command word, upper-cased, ends at a space, '/' or the line end
        mov di, cmd
        xor bx, bx              ; bx = 1 once a '.' has been seen
.w:     mov al, [si]
        cmp al, 13
        je .e
        cmp al, ' '
        je .e
        cmp al, '/'
        je .e
        cmp al, '.'
        jne .u
        mov bx, 1
.u:     cmp al, 'a'
        jb .s
        cmp al, 'z'
        ja .s
        sub al, 0x20
.s:     mov [di], al
        inc di
        inc si
        cmp di, cmd+60
        jb .w
.e:     mov byte [di], 0
        mov [rest], si
        mov [dot], bx
        mov bx, builtins
.t:     mov dx, [bx]
        test dx, dx
        jz .external
        push bx
        mov si, cmd
        mov di, dx
        call streq
        pop bx
        je .hit
        add bx, 4
        jmp .t
.hit:   call word [bx+2]
        jmp prompt

.external:
        call buildtail
        cmp word [dot], 0
        jne .go
        mov dx, cmd             ; no extension: try .COM, then .EXE
        mov si, ext_com
        call tryext
        jnc prompt
        cmp ax, 2
        jne .fail
        mov dx, cmd
        mov si, ext_exe
        call tryext
        jnc prompt
        jmp .fail
.go:    mov dx, cmd
        mov ax, 0x4B00
        mov bx, pb
        int 0x21
        jnc prompt
.fail:  mov dx, bad
        call puts
        jmp prompt

; tryext: run cmd+si (an extension) ; CF set and ax = error if it did not start
tryext: mov di, path
        mov bx, cmd
.c:     mov al, [bx]
        mov [di], al
        inc bx
        inc di
        test al, al
        jnz .c
        dec di
.x:     lodsb
        mov [di], al
        inc di
        test al, al
        jnz .x
        mov dx, path
        mov ax, 0x4B00
        mov bx, pb
        int 0x21
        push cs
        pop ds
        push cs
        pop es
        ret

; buildtail: the rest of the line becomes the child's command tail block
buildtail:
        mov si, [rest]
        mov di, tail+1
        xor cx, cx
.t:     mov al, [si]
        mov [di], al
        cmp al, 13
        je .d
        inc si
        inc di
        inc cx
        cmp cx, 126
        jb .t
        mov byte [di], 13
.d:     mov [tail], cl
        ret

; --- built-ins ---------------------------------------------------------

b_ver:  mov ah, 0x30
        int 0x21
        push ax
        mov dx, verm
        call puts
        pop ax
        push ax
        xor ah, ah
        call dec16
        mov dl, '.'
        call putc
        pop ax
        mov al, ah
        xor ah, ah
        call dec16
        jmp crlf

b_echo: mov si, [rest]
        cmp byte [si], ' '
        jne .l
        inc si
.l:     lodsb
        cmp al, 13
        je crlf
        mov dl, al
        call putc
        jmp .l

b_exit: mov ax, 0x4C00
        int 0x21

b_type: mov si, [rest]
        call skipsp
        mov di, path
.n:     mov al, [si]
        cmp al, 13
        je .z
        cmp al, ' '
        je .z
        mov [di], al
        inc si
        inc di
        jmp .n
.z:     mov byte [di], 0
        cmp di, path
        je .miss
        mov dx, path
        mov ax, 0x3D00
        int 0x21
        jc .miss
        mov bx, ax
.r:     mov dx, buf
        mov cx, 512
        mov ah, 0x3F
        int 0x21
        jc .close
        test ax, ax
        jz .close
        mov cx, ax
        push bx
        mov bx, 1
        mov dx, buf
        mov ah, 0x40
        int 0x21
        pop bx
        jmp .r
.close: mov ah, 0x3E
        int 0x21
        ret
.miss:  mov dx, nofile
        jmp puts

b_dir:  mov dx, star
        xor cx, cx
        mov ah, 0x4E
        int 0x21
        jc .done
.n:     mov si, dta+30
        xor bx, bx
.c:     lodsb
        test al, al
        jz .pad
        mov dl, al
        call putc
        inc bx
        jmp .c
.pad:   cmp bx, 14
        jae .size
        mov dl, ' '
        call putc
        inc bx
        jmp .pad
.size:  mov ax, [dta+26]
        mov dx, [dta+28]
        call dec32
        call crlf
        mov ah, 0x4F
        int 0x21
        jnc .n
.done:  ret

; --- helpers -----------------------------------------------------------

skipsp: cmp byte [si], ' '
        jne .d
        inc si
        jmp skipsp
.d:     ret

; streq: ZF set when the ASCIIZ strings at si and di are equal
streq:  mov al, [si]
        cmp al, [di]
        jne .d
        inc si
        inc di
        test al, al
        jnz streq
.d:     ret

putc:   mov ah, 2
        int 0x21
        ret
puts:   mov ah, 9
        int 0x21
        ret
crlf:   mov dx, nl
        jmp puts

dec16:  xor dx, dx              ; ax
dec32:  xor cx, cx              ; dx:ax
.l:     mov bx, 10
        mov si, ax
        mov ax, dx
        xor dx, dx
        div bx
        mov di, ax
        mov ax, si
        div bx
        push dx
        inc cx
        mov dx, di
        mov si, dx
        or si, ax
        jnz .l
.p:     pop dx
        add dl, '0'
        call putc
        loop .p
        ret

builtins:
        dw n_dir, b_dir, n_type, b_type, n_echo, b_echo, n_ver, b_ver, n_exit, b_exit, 0
n_dir:  db 'DIR', 0
n_type: db 'TYPE', 0
n_echo: db 'ECHO', 0
n_ver:  db 'VER', 0
n_exit: db 'EXIT', 0

banner: db 'DOSH command interpreter', 13, 10, '$'
ps:     db 'C:\>$'
nl:     db 13, 10, '$'
bad:    db 'Bad command or file name', 13, 10, '$'
nofile: db 'File not found', 13, 10, '$'
verm:   db 'DOSH DOS version $'
star:   db '*.*', 0
ext_com: db '.COM', 0
ext_exe: db '.EXE', 0

pb:     dw 0, tail, 0, fcb, 0, fcb, 0
fcb:    times 16 db 0
rest:   dw 0
dot:    dw 0
line:   db 126, 0
        times 130 db 0
cmd:    times 64 db 0
path:   times 70 db 0
tail:   times 130 db 0
dta:    times 48 db 0
buf:    times 512 db 0
