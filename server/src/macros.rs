// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

#[macro_export]
macro_rules! wrapped_scalar {
    ($name:ty) => {
        impl $crate::FromScalar<4> for $name {
            fn from_scalar(value: [u32; 4]) -> Self { Self($crate::FromScalar::from_scalar(value)) }
        }

        impl $crate::AsScalar<4> for $name {
            fn as_scalar(&self) -> [u32; 4] { self.0.as_scalar() }
        }
    };
}

/// Define a named set of message permissions, plus the blanket impl that makes every
/// permissions type granting those messages a member of it.
///
/// The set always requires [`CheckedPermissions`](crate::CheckedPermissions); listing other
/// sets after the colon composes them.
///
/// ```rust,ignore
/// permission_set!(pub trait ThemePermissions { GetSystemTheme, SetSystemTheme });
///
/// permission_set!(pub trait FileBackedPermissions: DurableFilePermissions {
///     CreateDirMessage, CloseDir
/// });
/// ```
#[macro_export]
macro_rules! permission_set {
    (
        $(#[doc = $doc:expr])*
        $vis:vis trait $name:ident $(: $($set:path),+)? {
            $($msg:path),+ $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        $vis trait $name:
            $crate::CheckedPermissions
            $($(+ $set)+)?
            $(+ $crate::MessageAllowed<$msg>)+
        {
        }

        impl<P> $name for P
        where
            P: $crate::CheckedPermissions
                $($(+ $set)+)?
                $(+ $crate::MessageAllowed<$msg>)+,
        {
        }
    };
}
