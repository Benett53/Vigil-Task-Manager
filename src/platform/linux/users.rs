//! uid -> user name resolution (works with NSS backends such as LDAP/SSSD).

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct UserCache {
    names: HashMap<u32, Arc<str>>,
}

impl UserCache {
    pub fn name(&mut self, uid: u32) -> Arc<str> {
        self.names
            .entry(uid)
            .or_insert_with(|| Arc::from(lookup(uid).unwrap_or_else(|| uid.to_string())))
            .clone()
    }
}

pub fn lookup(uid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: getpwuid_r writes into `pwd` and `buf`, both owned and sized here.
        unsafe {
            let mut pwd: libc::passwd = std::mem::zeroed();
            let mut result: *mut libc::passwd = std::ptr::null_mut();
            let rc = libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            );
            if rc == libc::ERANGE && buf.len() < 1 << 20 {
                buf.resize(buf.len() * 4, 0);
                continue;
            }
            if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
                return None;
            }
            return Some(
                std::ffi::CStr::from_ptr(pwd.pw_name)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

/// Full name from the GECOS field, if set.
pub fn full_name(uid: u32) -> Option<String> {
    let mut buf = vec![0u8; 16384];
    // SAFETY: see `lookup`.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() || pwd.pw_gecos.is_null() {
            return None;
        }
        let gecos = std::ffi::CStr::from_ptr(pwd.pw_gecos)
            .to_string_lossy()
            .into_owned();
        let name = gecos.split(',').next().unwrap_or("").trim().to_owned();
        if name.is_empty() { None } else { Some(name) }
    }
}
