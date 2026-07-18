pub(crate) const SERVER_NAME_TRNG: &[u8] = b"trng-server"; // depended upon by getrandom, do not change

/// These opcode numbers are partially baked into the `getrandom` library --
/// which kind of acts as a `std`-lib-ish style interface for the trng, so,
/// by design it can't have a dependency on this crate :-/
#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive, Debug)]
pub(crate) enum Opcode {
    /// Get two 32-bit words of TRNG data
    GetTrng = 0,

    /// Fill a buffer with random data. Buffer is expected to be an array of u32s,
    /// and the `valid` field of the memory message is the number of u32s to get.
    FillTrng = 1,

    /// Diagnostic access to raw 12-bit avalanche ADC samples. Buffer is expected
    /// to be an array of u32s; each returned value uses bits 0..=11.
    FillAvalancheRaw = 2,
}
