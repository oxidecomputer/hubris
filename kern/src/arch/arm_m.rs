use crate::task;

#[derive(Debug)]
pub struct SavedState {
    r4: u32,
    r5: u32,
    r6: u32,
    r7: u32,
    r8: u32,
    r9: u32,
    r10: u32,
    r11: u32,
    psp: u32,
}

impl task::ArchState for SavedState {
    fn stack_pointer(&self) -> u32 {
        self.psp
    }

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32 {
        self.r4
    }
    fn arg1(&self) -> u32 {
        self.r5
    }
    fn arg2(&self) -> u32 {
        self.r6
    }
    fn arg3(&self) -> u32 {
        self.r7
    }
    fn arg4(&self) -> u32 {
        self.r8
    }
    fn arg5(&self) -> u32 {
        self.r9
    }
    fn arg6(&self) -> u32 {
        self.r10
    }
    fn arg7(&self) -> u32 {
        self.r11
    }

    /// Writes syscall return argument 0.
    fn ret0(&mut self, x: u32) {
        self.r4 = x
    }
    fn ret1(&mut self, x: u32) {
        self.r5 = x
    }
    fn ret2(&mut self, x: u32) {
        self.r6 = x
    }
    fn ret3(&mut self, x: u32) {
        self.r7 = x
    }
    fn ret4(&mut self, x: u32) {
        self.r8 = x
    }
    fn ret5(&mut self, x: u32) {
        self.r9 = x
    }
}
