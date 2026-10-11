//! Windows-only pieces the shell would otherwise need native interop for:
//! keyboard/mouse capture and the composition surface the video draws into.

pub mod capture;

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

#[link(name = "dcomp")]
unsafe extern "system" {
    fn DCompositionCreateSurfaceHandle(
        desired_access: u32,
        security_attributes: *const c_void,
        surface_handle: *mut HANDLE,
    ) -> i32;
}

const COMPOSITIONOBJECT_ALL_ACCESS: u32 = 0x0003;

/// A DirectComposition surface handle for `NativeSurface::CompositionHandle`.
/// The shell shows it with `ICompositorInterop::CreateCompositionSurfaceForHandle`.
pub fn create_composition_surface() -> Result<u64, String> {
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: plain out-parameter call.
    let hr = unsafe { DCompositionCreateSurfaceHandle(COMPOSITIONOBJECT_ALL_ACCESS, std::ptr::null(), &mut handle) };
    if hr < 0 || handle.is_null() {
        return Err(format!("DCompositionCreateSurfaceHandle failed: {hr:#x}"));
    }
    Ok(handle as u64)
}

/// Close a handle from `create_composition_surface` (after the video using
/// it has stopped and the shell has released its surface).
pub fn close_composition_surface(handle: u64) {
    if handle != 0 {
        // SAFETY: the caller passes a handle it got from us, once.
        unsafe { CloseHandle(handle as usize as HANDLE) };
    }
}
