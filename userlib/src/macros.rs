#[cfg(feature = "log-itm")]
#[macro_export]
macro_rules! sys_log {
    ($s:expr) => {
        unsafe {
            let stim = &mut (*cortex_m::peripheral::ITM::ptr()).stim[1];
            cortex_m::iprintln!(stim, $s);
        }
    };
    ($s:expr, $($tt:tt)*) => {
        unsafe {
            let stim = &mut (*cortex_m::peripheral::ITM::ptr()).stim[1];
            cortex_m::iprintln!(stim, $s, $($tt)*);
        }
    };
}

#[cfg(feature = "log-semihosting")]
#[macro_export]
macro_rules! sys_log {
    ($s:expr) => {
        { let _ = cortex_m_semihosting::hprintln!($s); }
    };
    ($s:expr, $($tt:tt)*) => {
        { let _ = cortex_m_semihosting::hprintln!($s, $($tt)*); }
    };
}

#[cfg(not(any(feature = "log-semihosting", feature = "log-itm")))]
#[macro_export]
macro_rules! sys_log {
    ($s:expr) => {
        compile_error!(concat!(
            "to use sys_log! must enable either ",
            "'log-semihosting' or 'log-itm' feature"
        ))
    };
    ($s:expr, $($tt:tt)*) => {
        compile_error!(concat!(
            "to use sys_log! must enable either ",
            "'log-semihosting' or 'log-itm' feature"
        ))
    };
}

#[macro_export]
macro_rules! declare_task {
    ($var:ident, $task_name:ident) => {
        #[cfg(not(feature = "standalone"))]
        const $var: Task = Task::$task_name;

        #[cfg(feature = "standalone")]
        const $var: Task = Task::anonymous;
    };
}
