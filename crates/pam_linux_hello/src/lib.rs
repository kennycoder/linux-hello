use std::ffi::{CStr, CString};
use std::io::{BufRead, BufReader, Write};
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub const PAM_SUCCESS: c_int = 0;
pub const PAM_AUTH_ERR: c_int = 7;
pub const PAM_AUTHINFO_UNAVAIL: c_int = 9;
pub const PAM_USER_UNKNOWN: c_int = 10;
pub const PAM_IGNORE: c_int = 25;

pub const PAM_USER: c_int = 2;
pub const PAM_CONV: c_int = 5;
pub const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
pub struct PamMessage {
    pub msg_style: c_int,
    pub msg: *const c_char,
}

#[repr(C)]
pub struct PamResponse {
    pub resp: *mut c_char,
    pub resp_retcode: c_int,
}

#[repr(C)]
pub struct PamConv {
    pub conv: Option<
        extern "C" fn(
            num_msg: c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    pub appdata_ptr: *mut c_void,
}

pub enum PamHandle {}

extern "C" {
    fn pam_get_user(
        pamh: *const PamHandle,
        user: *mut *const c_char,
        prompt: *const c_char,
    ) -> c_int;

    fn pam_get_item(
        pamh: *const PamHandle,
        item_type: c_int,
        item: *mut *const c_void,
    ) -> c_int;
}

fn send_pam_info(pamh: *const PamHandle, text: &str) {
    unsafe {
        let mut conv_ptr: *const c_void = std::ptr::null();
        if pam_get_item(pamh, PAM_CONV, &mut conv_ptr) != PAM_SUCCESS || conv_ptr.is_null() {
            return;
        }

        let conv = &*(conv_ptr as *const PamConv);
        if let Some(conv_fn) = conv.conv {
            let c_msg = match CString::new(text) {
                Ok(s) => s,
                Err(_) => return,
            };

            let msg = PamMessage {
                msg_style: PAM_TEXT_INFO,
                msg: c_msg.as_ptr(),
            };

            let mut msg_ptr = &msg as *const PamMessage;
            let mut resp_ptr: *mut PamResponse = std::ptr::null_mut();

            conv_fn(1, &mut msg_ptr, &mut resp_ptr, conv.appdata_ptr);

            if !resp_ptr.is_null() {
                if !(*resp_ptr).resp.is_null() {
                    libc::free((*resp_ptr).resp as *mut c_void);
                }
                libc::free(resp_ptr as *mut c_void);
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    let mut username_ptr: *const c_char = std::ptr::null();
    let status = pam_get_user(pamh, &mut username_ptr, std::ptr::null());
    if status != PAM_SUCCESS || username_ptr.is_null() {
        return PAM_USER_UNKNOWN;
    }

    let username = match CStr::from_ptr(username_ptr).to_str() {
        Ok(s) => s,
        Err(_) => return PAM_AUTH_ERR,
    };

    // Parse options from argv
    let mut socket_path = "/run/linux-hello.sock".to_string();
    let mut timeout_ms: u64 = 3500;
    let mut quiet = false;

    for i in 0..argc as isize {
        let arg_ptr = *argv.offset(i);
        if arg_ptr.is_null() {
            continue;
        }
        if let Ok(arg) = CStr::from_ptr(arg_ptr).to_str() {
            if let Some(s) = arg.strip_prefix("socket=") {
                socket_path = s.to_string();
            } else if let Some(t) = arg.strip_prefix("timeout=") {
                if let Ok(ms) = t.parse::<u64>() {
                    timeout_ms = ms;
                }
            } else if arg == "quiet" {
                quiet = true;
            }
        }
    }

    // If socket does not exist, daemon is offline: fall back quietly to password
    if !Path::new(&socket_path).exists() {
        return PAM_IGNORE;
    }

    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return PAM_IGNORE,
    };

    let _ = stream.set_read_timeout(Some(Duration::from_millis(timeout_ms + 1000)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));

    if !quiet {
        send_pam_info(pamh, "[Linux-Hello] Identifying face via IR camera...");
    }

    let req = format!(
        "{{\"action\":\"authenticate\",\"username\":\"{}\",\"timeout_ms\":{}}}\n",
        username, timeout_ms
    );

    let mut stream = stream;
    if stream.write_all(req.as_bytes()).is_err() {
        return PAM_AUTHINFO_UNAVAIL;
    }

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    if reader.read_line(&mut response_line).is_err() || response_line.is_empty() {
        return PAM_AUTHINFO_UNAVAIL;
    }

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&response_line) {
        if val.get("success").and_then(|v| v.as_bool()) == Some(true) {
            if !quiet {
                send_pam_info(pamh, "[Linux-Hello] Face authentication successful!");
            }
            return PAM_SUCCESS;
        }
    }

    PAM_AUTH_ERR
}

#[no_mangle]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}
