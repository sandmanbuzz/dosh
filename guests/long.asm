; LONG.COM - a computation that takes seconds, so an operator can suspend it
; part way and see it finish with the right answer after it resumes. Sums
; 1..N six times over (mod 2^32) with interrupts enabled, and prints the
; total as eight hex digits.
        org 0x100
N       equ 400000000
ROUNDS  equ 6
        xor edi, edi
        mov si, ROUNDS
.round: mov ecx, N
        xor eax, eax
.spin:  add eax, ecx
        dec ecx
        jnz .spin
        add edi, eax
        dec si
        jnz .round

        mov eax, edi
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
