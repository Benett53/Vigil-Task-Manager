//! Executable icons via the shell (the same icons Explorer and Task Manager show).

use super::wide;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

/// Prepares the calling thread for shell calls. Call once per icon thread.
pub fn init_thread() {
    // SAFETY: plain COM initialisation; failure only means it was already initialised.
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
    }
}

/// 32×32 RGBA icon of an executable.
pub fn exe_icon(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    let path = wide(path);
    // SAFETY: `sfi` is plain data sized for the call; the returned icon is destroyed below.
    unsafe {
        let mut sfi: SHFILEINFOW = zeroed();
        let ok = SHGetFileInfoW(
            path.as_ptr(),
            0,
            &mut sfi,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || sfi.hIcon.is_null() {
            return None;
        }
        let out = icon_rgba(sfi.hIcon);
        DestroyIcon(sfi.hIcon);
        out
    }
}

unsafe fn bitmap_bgra(dc: HDC, bmp: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    // SAFETY: the buffer is sized for a 32 bpp top-down DIB of w×h pixels.
    unsafe {
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            dc,
            bmp,
            0,
            h as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut bi,
            DIB_RGB_COLORS,
        );
        (lines == h).then_some(buf)
    }
}

unsafe fn icon_rgba(icon: HICON) -> Option<(usize, usize, Vec<u8>)> {
    // SAFETY: GetIconInfo hands us two bitmaps that we own and delete.
    unsafe {
        let mut info: ICONINFO = zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        let result = (|| {
            if info.hbmColor.is_null() {
                return None;
            }
            let mut bm: BITMAP = zeroed();
            if GetObjectW(
                info.hbmColor,
                size_of::<BITMAP>() as i32,
                &mut bm as *mut BITMAP as *mut c_void,
            ) == 0
            {
                return None;
            }
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            if w <= 0 || h <= 0 || w > 256 || h > 256 {
                return None;
            }
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            let mut px = bitmap_bgra(dc, info.hbmColor, w, h);
            // Icons without an alpha channel use the AND mask for transparency.
            if let Some(px) = px.as_mut()
                && px.as_chunks::<4>().0.iter().all(|p| p[3] == 0)
            {
                match bitmap_bgra(dc, info.hbmMask, w, h) {
                    Some(mask) => {
                        for (p, m) in px
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(mask.as_chunks::<4>().0)
                        {
                            p[3] = if m[0] == 0 { 255 } else { 0 };
                        }
                    }
                    None => px
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .for_each(|p| p[3] = 255),
                }
            }
            DeleteDC(dc);
            let mut px = px?;
            for p in px.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
            Some((w as usize, h as usize, px))
        })();
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
        result
    }
}
