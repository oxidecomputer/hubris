cfg_if::cfg_if! {
    if #[cfg(target_board = "nucleo-h743zi2")] {
        mod nucleo_h743zi2;
        pub use nucleo_h743zi2::{configure_ethernet_pins, configure_phy};
    } else if #[cfg(target_board = "sidecar-1")] {
        mod sidecar_1;
        pub use sidecar_1::{configure_ethernet_pins, configure_phy};
    } else {
        compile_error!("Board is not supported by the task/net");
    }
}
