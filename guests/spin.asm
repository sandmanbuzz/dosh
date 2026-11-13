; SPIN.COM - a billion and a half instructions without asking anyone for
; anything, to show that guest code runs on the processor rather than under
; an interpreter. It runs with interrupts disabled, the most selfish a real-
; mode program can be; the supervisor must be able to take the processor
; back regardless. Prints the sum of 1..N as eight hex digits.
        org 0x100
N       equ 500000000
        cli
        mov ecx, N
        xor eax, eax
.spin:  add eax, ecx
        dec ecx
        jnz .spin

        mov cx, 8
.digit: rol eax, 4
        mov dl, al
        and dl, 0x0F
        add dl, '0'
        cmp dl, '9'
        jbe .put
        add dl, 'A' - '0' - 10
.put:   push eax
        mov ah, 0x02
        int 0x21
        pop eax
        loop .digit
        mov ax, 0x4C00
        int 0x21
