; EXEC.COM - start child programs with INT 21h/4Bh, then collect their
; return codes with 4Dh. Proves: the parent gives memory back (4Ah), the
; child inherits the console, gets the command tail and a PSP whose parent is
; us, and the parent resumes with registers, stack and memory intact.
        org 0x100
        mov ah, 0x4A            ; keep 64 KiB: the rest is the children's
        mov bx, 0x1000
        int 0x21
        mov [pb+4], cs          ; fill the parameter block's segments
        mov [pb+8], cs
        mov [pb+12], cs
        mov bp, 0x1234          ; must survive the children
        mov dx, args
        call run
        mov dx, exit42
        call run
        cmp bp, 0x1234
        jne .bad
        mov dx, missing         ; a program that is not there: carry, error 2
        mov ax, 0x4B00
        mov bx, pb
        int 0x21
        jnc .bad
        cmp ax, 2
        jne .bad
        mov dx, ok
        mov ah, 9
        int 0x21
        mov ax, 0x4C00
        int 0x21
.bad:   mov dx, no
        mov ah, 9
        int 0x21
        mov ax, 0x4C01
        int 0x21

; run the program named at dx, print "code=NN" for its return code
run:    mov ax, 0x4B00
        mov bx, pb
        int 0x21
        jc .fail
        mov ah, 0x4D
        int 0x21
        aam
        add ax, 0x3030
        mov [num], ah
        mov [num+1], al
        mov dx, code
        mov ah, 9
        int 0x21
.fail:  ret

pb:     dw 0                    ; inherit the environment
        dw tail, 0              ; command tail
        dw fcb, 0               ; first FCB
        dw fcb, 0               ; second FCB
tail:   db 3, ' hi', 13
fcb:    times 16 db 0
args:   db 'ARGS.COM', 0
exit42: db 'EXIT42.COM', 0
missing: db 'NOPE.COM', 0
code:   db 'code='
num:    db '00', 13, 10, '$'
ok:     db 'parent resumed', 13, 10, '$'
no:     db 'FAILED', 13, 10, '$'
