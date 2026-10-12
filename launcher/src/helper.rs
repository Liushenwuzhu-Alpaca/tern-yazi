//! The helper mode the Yazi plugin runs through the bridge's `helper` path.

use std::ffi::OsString;
use std::io;

use crate::sys::{cstr, invalid, last_error};

// The Lua filesystem API only exposes overwriting rename. Keep collision
// refusal atomic at the existing Linux process boundary instead of stat+rename.
pub(crate) fn rename_no_replace(args: &[OsString]) -> io::Result<i32> {
    if args.len() != 2 {
        return Err(invalid("rename requires source and destination"));
    }
    let source = cstr(&args[0])?;
    let target = cstr(&args[1])?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(last_error());
    }
    Ok(0)
}
