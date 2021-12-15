#[cfg(not(target_board = "gemini-bu-1"))]
pub mod empty;

#[cfg(target_board = "gemini-bu-1")]
pub mod gemini_bu;
