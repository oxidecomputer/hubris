use crate::task;

/// On "kernel entry" we expect the simulator to send over a record containing
/// all args. We expect to send back all the return values when we "return to
/// user."
pub struct SavedState {
    args: [u32; 8],
    rets: [u32; 6],
}

impl task::ArchState for SavedState {
    fn stack_pointer(&self) -> u32 {
        // TODO: this is an argument for removing stack_pointer from ArchState.
        unimplemented!()
    }
    
    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32 {
        self.args[0]
    }
    fn arg1(&self) -> u32 {
        self.args[1]
    }
    fn arg2(&self) -> u32 {
        self.args[2]
    }
    fn arg3(&self) -> u32 {
        self.args[3]
    }
    fn arg4(&self) -> u32 {
        self.args[4]
    }
    fn arg5(&self) -> u32 {
        self.args[5]
    }
    fn arg6(&self) -> u32 {
        self.args[6]
    }
    fn arg7(&self) -> u32 {
        self.args[7]
    }

    /// Writes syscall return argument 0.
    fn ret0(&mut self, x: u32) {
        self.rets[0] = x
    }
    fn ret1(&mut self, x: u32) {
        self.rets[1] = x
    }
    fn ret2(&mut self, x: u32) {
        self.rets[2] = x
    }
    fn ret3(&mut self, x: u32) {
        self.rets[3] = x
    }
    fn ret4(&mut self, x: u32) {
        self.rets[4] = x
    }
    fn ret5(&mut self, x: u32) {
        self.rets[5] = x
    }
}

