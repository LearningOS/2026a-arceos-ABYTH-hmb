#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]

#[cfg(feature = "axstd")]
use axstd::println;

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    // Print the greeting with ANSI color escape codes:
    // `\x1b[1;32;44m` = bold, green foreground, blue background.
    println!("\x1b[1;32;44m[WithColor]: Hello, Arceos!\x1b[0m");
}
