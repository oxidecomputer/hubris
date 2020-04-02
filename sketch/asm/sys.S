.syntax unified

.global _sys_send
.thumb_func
_sys_send:
    svc 0
    bx lr
