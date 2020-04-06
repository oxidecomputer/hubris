.syntax unified

.global _sys_send
.thumb_func
_sys_send:
    svc 0
    bx lr

.global _sys_receive
.thumb_func
_sys_receive:
    svc 1
    bx lr

.global _sys_reply
.thumb_func
_sys_reply:
    svc 2
    bx lr

.global _sys_notmask
.thumb_func
_sys_notmask:
    svc 3
    bx lr

.global _sys_borrow_size
.thumb_func
_sys_borrow_size:
    svc 4
    bx lr

.global _sys_borrow_write
.thumb_func
_sys_borrow_write:
    svc 5
    bx lr

