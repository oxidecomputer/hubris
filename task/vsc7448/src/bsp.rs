cfg_if::cfg_if! {
    if #[cfg(target_board = "gemini-bu-1")] {
        pub mod gemini_bu;
    } else {
        pub mod empty;
    }
}
