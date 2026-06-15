#[cfg(keyos)]
mod arm;
#[cfg(keyos)]
pub use arm::*;

#[cfg(not(keyos))]
pub mod hosted;
#[cfg(not(keyos))]
pub use hosted::*;
