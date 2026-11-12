.section .text
.globl _start
_start:
    mov $60, %eax
    mov $42, %edi
    syscall
    ud2

.section .note.GNU-stack,"",@progbits
