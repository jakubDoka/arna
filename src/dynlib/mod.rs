use std::{ffi::CString, mem, path::Path};

pub mod hot;

pub struct DynamicLibrary {
    handle: *mut u8,
}

impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        dl::check_for_errors_in(
            || unsafe { dl::close(self.handle) },
            |msg| panic!("{msg}"),
        );
    }
}

impl DynamicLibrary {
    /// Lazily open a dynamic library. When passed None it gives a
    /// handle to the calling process
    pub fn open(filename: Option<&Path>) -> Result<DynamicLibrary, String> {
        let maybe_library = dl::open(filename.map(|path| path.as_os_str()));

        match maybe_library {
            Err(err) => Err(err),
            Ok(handle) => Ok(DynamicLibrary { handle: handle }),
        }
    }

    /// Access the value at the symbol of the dynamic library
    pub unsafe fn symbol<T>(&self, symbol: &str) -> Result<*mut T, String> {
        // This function should have a lifetime constraint of 'a on
        // T but that feature is still unimplemented

        let raw_string = CString::new(symbol).unwrap();
        let maybe_symbol_value = dl::check_for_errors_in(
            || unsafe { dl::symbol(self.handle, raw_string.as_ptr()) },
            str::to_owned,
        );

        // The value must not be constructed if there is an error so
        // the destructor does not run.
        match maybe_symbol_value {
            Err(err) => Err(err),
            Ok(symbol_value) => Ok(unsafe { mem::transmute(symbol_value) }),
        }
    }
}

#[cfg(not(miri))]
#[cfg(all(test, not(target_os = "ios")))]
mod test {
    use {
        super::*,
        std::{mem, path::Path},
    };

    #[test]
    #[cfg_attr(any(windows, target_os = "android"), ignore)]
    fn test_loading_cosine() {
        // The math library does not need to be loaded since it is already
        // statically linked in
        let libm = match DynamicLibrary::open(None) {
            Err(error) => panic!("Could not load self as module: {}", error),
            Ok(libm) => libm,
        };

        let cosine: extern "C" fn(f64) -> f64 = unsafe {
            match libm.symbol("cos") {
                Err(error) => panic!("Could not load function cos: {}", error),
                Ok(cosine) => mem::transmute::<*mut u8, _>(cosine),
            }
        };

        let argument = 0.0;
        let expected_result = 1.0;
        let result = cosine(argument);
        if result != expected_result {
            panic!(
                "cos({}) != {} but equaled {} instead",
                argument, expected_result, result
            )
        }
    }

    #[test]
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd"
    ))]
    fn test_errors_do_not_crash() {
        let path = Path::new("/dev/null");
        match DynamicLibrary::open(Some(&path)) {
            Err(_) => {}
            Ok(_) => panic!("Successfully opened the empty library."),
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd"
))]
mod dl {
    use std::{
        ffi::{CStr, CString, OsStr},
        os::unix::ffi::OsStrExt,
        ptr, str,
    };

    pub fn open(filename: Option<&OsStr>) -> Result<*mut u8, String> {
        check_for_errors_in(
            || unsafe {
                match filename {
                    Some(filename) => open_external(filename),
                    None => open_internal(),
                }
            },
            str::to_owned,
        )
    }

    const LAZY: core::ffi::c_int = 1;

    unsafe fn open_external(filename: &OsStr) -> *mut u8 {
        let s = CString::new(filename.as_bytes()).unwrap(); //to_cstring().unwrap();
        unsafe { dlopen(s.as_ptr(), LAZY) as *mut u8 }
    }

    unsafe fn open_internal() -> *mut u8 {
        unsafe { dlopen(ptr::null(), LAZY) as *mut u8 }
    }

    pub fn check_for_errors_in<T, E, F, H>(f: F, handler: H) -> Result<T, E>
    where
        F: FnOnce() -> T,
        H: FnOnce(&str) -> E,
    {
        unsafe {
            let result = f();

            let last_error = dlerror() as *const core::ffi::c_char;
            if last_error.is_null() {
                Ok(result)
            } else {
                let s = CStr::from_ptr(last_error).to_bytes();
                Err(handler(str::from_utf8(s).unwrap()))
            }
        }
    }

    pub unsafe fn symbol(
        handle: *mut u8,
        symbol: *const core::ffi::c_char,
    ) -> *mut u8 {
        unsafe { dlsym(handle as *mut core::ffi::c_void, symbol) as *mut u8 }
    }
    pub unsafe fn close(handle: *mut u8) {
        unsafe { dlclose(handle as *mut core::ffi::c_void) };
    }

    unsafe extern "C" {
        fn dlopen(
            filename: *const core::ffi::c_char,
            flag: core::ffi::c_int,
        ) -> *mut core::ffi::c_void;
        fn dlerror() -> *mut core::ffi::c_char;
        fn dlsym(
            handle: *mut core::ffi::c_void,
            symbol: *const core::ffi::c_char,
        ) -> *mut core::ffi::c_void;
        fn dlclose(handle: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
}

#[cfg(target_os = "windows")]
mod dl {
    use std::{
        core::ffi::consts::os::extra::ERROR_CALL_NOT_IMPLEMENTED,
        ffi::OsStr,
        iter::Iterator,
        ops::FnOnce,
        option::Option::{self, None, Some},
        os::windows::prelude::*,
        ptr,
        result::{
            Result,
            Result::{Err, Ok},
        },
        string::String,
        sys::{c::compat::kernel32::SetThreadErrorMode, os},
        vec::Vec,
    };

    pub fn open(filename: Option<&OsStr>) -> Result<*mut u8, String> {
        // disable "dll load failed" error dialog.
        let mut use_thread_mode = true;
        let prev_error_mode = unsafe {
            // SEM_FAILCRITICALERRORS 0x01
            let new_error_mode = 1;
            let mut prev_error_mode = 0;
            // Windows >= 7 supports thread error mode.
            let result =
                SetThreadErrorMode(new_error_mode, &mut prev_error_mode);
            if result == 0 {
                let err = os::errno();
                if err as core::ffi::c_int == ERROR_CALL_NOT_IMPLEMENTED {
                    use_thread_mode = false;
                    // SetThreadErrorMode not found. use fallback solution:
                    // SetErrorMode() Note that SetErrorMode is process-wide so
                    // this can cause race condition!  However, since even
                    // Windows APIs do not care of such problem (#20650), we
                    // just assume SetErrorMode race is not a great deal.
                    prev_error_mode = SetErrorMode(new_error_mode);
                }
            }
            prev_error_mode
        };

        unsafe {
            SetLastError(0);
        }

        let result = match filename {
            Some(filename) => {
                let filename_str: Vec<_> =
                    filename.encode_wide().chain(Some(0).into_iter()).collect();
                let result = unsafe {
                    LoadLibraryW(
                        filename_str.as_ptr() as *const core::ffi::c_void
                    )
                };
                // beware: Vec/String may change errno during drop!
                // so we get error here.
                if result == ptr::null_mut() {
                    let errno = os::errno();
                    Err(os::error_string(errno))
                } else {
                    Ok(result as *mut u8)
                }
            }
            None => {
                let mut handle = ptr::null_mut();
                let succeeded = unsafe {
                    GetModuleHandleExW(
                        0 as core::ffi::DWORD,
                        ptr::null(),
                        &mut handle,
                    )
                };
                if succeeded == core::ffi::FALSE {
                    let errno = os::errno();
                    Err(os::error_string(errno))
                } else {
                    Ok(handle as *mut u8)
                }
            }
        };

        unsafe {
            if use_thread_mode {
                SetThreadErrorMode(prev_error_mode, ptr::null_mut());
            } else {
                SetErrorMode(prev_error_mode);
            }
        }

        result
    }

    pub fn check_for_errors_in<T, F>(f: F) -> Result<T, String>
    where
        F: FnOnce() -> T,
    {
        unsafe {
            SetLastError(0);

            let result = f();

            let error = os::errno();
            if 0 == error {
                Ok(result)
            } else {
                Err(format!("Error code {}", error))
            }
        }
    }

    pub unsafe fn symbol(
        handle: *mut u8,
        symbol: *const core::ffi::c_char,
    ) -> *mut u8 {
        GetProcAddress(handle as *mut core::ffi::c_void, symbol) as *mut u8
    }
    pub unsafe fn close(handle: *mut u8) {
        FreeLibrary(handle as *mut core::ffi::c_void);
        ()
    }

    #[allow(non_snake_case)]
    extern "system" {
        fn SetLastError(error: core::ffi::size_t);
        fn LoadLibraryW(
            name: *const core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        fn GetModuleHandleExW(
            dwFlags: core::ffi::DWORD,
            name: *const u16,
            handle: *mut *mut core::ffi::c_void,
        ) -> core::ffi::BOOL;
        fn GetProcAddress(
            handle: *mut core::ffi::c_void,
            name: *const core::ffi::c_char,
        ) -> *mut core::ffi::c_void;
        fn FreeLibrary(handle: *mut core::ffi::c_void);
        fn SetErrorMode(uMode: core::ffi::c_uint) -> core::ffi::c_uint;
    }
}
