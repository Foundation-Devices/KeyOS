/// Do not modify the discriminants in this structure. They are used in `libstd` directly.
#[repr(usize)]
#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive, Debug, Clone, Copy)]
pub enum Opcode {
    /// Get the elapsed time in nanoseconds
    ///
    /// *arg1*(lo) + *arg2*(hi): nanoseconds elapsed since boot
    Elapsed = 0,

    /// Sleep for the specified numer of nanoseconds
    /// *arg1*(lo) + *arg2*(hi)
    Sleep = 1,

    /// Wait for a given condition to be signalled
    ///
    /// # Arguments
    ///
    /// *arg1*: An integer of some sort, such as the address of the Condvar
    /// *arg2*(lo) + *arg3*(hi): The number of milliseconds to wait, or 0 to wait forever
    WaitForCondition = 8,

    /// Notify a condition
    ///
    /// # Arguments
    ///
    /// *arg1*: An integer of some sort, such as the address of the Condvar
    /// *arg2*: The number of conditions to notify
    NotifyCondition = 9,

    /// Get the current system time
    /// # Return value (Scalar1)
    ///
    /// *arg1*(lo) + *arg2*(hi) nanoseconds elapsed since the unix epoch
    GetSystemTime = 12,

    /// Request a callback in a set amount of time
    RequestCallback = 13,

    // ------ Privileged calls ------
    /// Set the current system time
    ///
    /// # Arguments
    ///
    /// *arg1*(lo) + *arg2*(hi) nanoseconds elapsed since the unix epoch
    SetSystemTime = 20,

    // ------ Internal-only calls -------
    /// System event telling us a client has disconnected
    Disconnected = 30,

    /// The timer interrupt was called
    TimerInterrupt = 31,

    /// Periodic watchdog reset callback
    #[cfg(keyos)]
    WatchdogReset = 32,

    /// Invalid call -- an error occurred decoding the opcode
    InvalidCall = u32::MAX as usize,
}
