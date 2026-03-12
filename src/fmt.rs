// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(unused_macros)]

#[collapse_debuginfo(yes)]
macro_rules! trace {
    ($s:literal $(, $x:expr)* $(,)?) => {
        { ::log::trace!($s $(, $x)*) }
    };
}

#[collapse_debuginfo(yes)]
macro_rules! debug {
    ($s:literal $(, $x:expr)* $(,)?) => {
        { ::log::debug!($s $(, $x)*) }
    };
}

#[collapse_debuginfo(yes)]
macro_rules! info {
    ($s:literal $(, $x:expr)* $(,)?) => {
        { ::log::info!($s $(, $x)*) }
    };
}

#[collapse_debuginfo(yes)]
macro_rules! warn {
    ($s:literal $(, $x:expr)* $(,)?) => {
        { ::log::warn!($s $(, $x)*) }
    };
}

#[collapse_debuginfo(yes)]
macro_rules! error {
    ($s:literal $(, $x:expr)* $(,)?) => {
        { ::log::error!($s $(, $x)*) }
    };
}
