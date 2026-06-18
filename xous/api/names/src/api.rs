pub const NAME_MAX_LENGTH: usize = 64;

#[derive(num_derive::FromPrimitive, num_derive::ToPrimitive)]
#[repr(C)]
pub enum Opcode {
    /// Create a new server with the given name and return its SID.
    Register = 0,

    /// Add a manifest file
    ///
    /// # Message Types
    ///
    /// * MutableLend
    ///
    /// # Arguments
    ///
    /// The memory being pointed to should be a `&[u8]` of a JSON serialized manifest,
    /// and its length should be specified in the `valid` field.
    AddManifest = 1,

    /// Remove a previously added manifest by app ID.
    ///
    /// # Message Types
    ///
    /// * MutableLend
    ///
    /// # Arguments
    ///
    /// The memory being pointed to should be the 16-byte app ID whose manifest state
    /// should be removed, and its length should be specified in the `valid` field.
    RemoveManifest = 2,

    /// Connect to a Server, blocking if the Server does not exist. When the Server is started,
    /// return with either the CID or an AuthenticationRequest
    ///
    /// # Message Types
    ///
    /// * MutableLend
    ///
    /// # Arguments
    ///
    /// The memory being pointed to should be a &str, and the length of the string should
    /// be specified in the `valid` field.
    BlockingConnect = 6,

    /// Connect to a Server, returning the connection ID or an authentication request if
    /// it exists, and returning ServerNotFound if it does not exist.
    ///
    /// # Message Types
    ///
    /// * MutableLend
    ///
    /// # Arguments
    ///
    /// The memory being pointed to should be a &str, and the length of the string should
    /// be specified in the `valid` field.
    TryConnect = 7,
}
