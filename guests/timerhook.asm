; TIMERHOOK.COM - the BIOS tick reaches a program's own INT 1Ch hook. The
; BIOS default INT 08h handler calls INT 1Ch on every tick; this program
; hooks INT 1Ch only and never touches INT 08h. It idles in HLT until the
; hook has run three times, then prints HOOK OK and exits 0.
        org 0x100
        mov ax, 0x251C
        mov dx, hook
        int 0x21
        sti
idle:
        hlt
        cmp word [hooked], 3
        jb idle
        cli
        mov ah, 0x09
        mov dx, msg
        int 0x21
        mov ax, 0x4C00
        int 0x21
hook:
        inc word [cs:hooked]
        iret
hooked: dw 0
msg:    db 'HOOK OK', 13, 10, '$'
