#![cfg_attr(not(feature = "std"), no_std)]

use core::{
    alloc::Layout,
    cell::Cell,
    marker::{PhantomData, PhantomPinned},
    mem::{MaybeUninit, transmute},
    ops::Deref,
    pin::Pin,
    ptr::{NonNull, copy_nonoverlapping},
};

use core::ptr::slice_from_raw_parts_mut;

use core::slice;

#[cfg(feature = "alloc")]
extern crate alloc;

pub struct Checkpoint<'a> {
    arna: NonNull<Arna<'a>>,
    prev_pos: usize,
    depth: usize,
}

impl<'a> Checkpoint<'a> {
    pub fn checkpoit(&self) -> Self {
        let arna: &Arna = self.deref();
        unsafe { Pin::new_unchecked(arna).checkpoit_unchecked() }
    }
}

impl<'a> Deref for Checkpoint<'a> {
    type Target = Arna<'a>;

    fn deref(&self) -> &Self::Target {
        let arna = unsafe { self.arna.as_ref() };
        assert_eq!(self.depth, arna.depth.get());
        arna
    }
}

impl Drop for Checkpoint<'_> {
    fn drop(&mut self) {
        let arna: &Arna = self.deref();
        arna.pos.set(self.prev_pos);
        arna.depth.update(|d| d - 1);
    }
}

#[derive(Default)]
pub struct Arna<'a> {
    pos: Cell<usize>,
    commited: Cell<usize>,
    ptr: *mut u8,
    cap: usize,
    depth: Cell<usize>,

    borrow: PhantomData<&'a mut [u8]>,
    _unpin: PhantomPinned,
}

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackingMode {
    #[cfg(feature = "virtual")]
    Mmap = 0,
    Slice = 1,
    #[cfg(feature = "alloc")]
    Box = 2,
}

impl<'a> Arna<'a> {
    pub fn backing_mode(&self) -> BackingMode {
        let vl = (self.commited.get() as isize - self.cap as isize).max(0);
        debug_assert!(vl < 3);
        unsafe { transmute(vl) }
    }

    #[cfg(feature = "virtual")]
    pub fn clear_and_decommit(&mut self) {
        // SAFETY: the checkpoits can only be created from the pinned arena, that means we
        // cant call this
        assert_eq!(self.backing_mode(), BackingMode::Mmap);
        unsafe {
            use core::slice::from_raw_parts_mut;

            virtual_mem::decommit(from_raw_parts_mut(self.ptr, self.commited.get()))
                .expect("this should not fail considering the invariants")
        };
        self.commited.set(0);
        self.pos.set(0);
    }

    pub fn checkpoit(self: Pin<&Self>) -> Checkpoint<'a> {
        assert!(self.depth.get() == 0);
        unsafe { self.checkpoit_unchecked() }
    }

    pub unsafe fn checkpoit_unchecked(self: Pin<&Self>) -> Checkpoint<'a> {
        self.depth.update(|d| d + 1);
        Checkpoint {
            arna: self.get_ref().into(),
            depth: self.depth.get(),
            prev_pos: self.pos.get(),
        }
    }

    pub fn alloc_copy<T: Copy>(&self, value: &[T]) -> &mut [T] {
        let alloc = self.alloc_uninit(value.len());
        unsafe { copy_nonoverlapping(value.as_ptr(), alloc.as_ptr() as _, value.len()) };
        unsafe { alloc.assume_init_mut() }
    }

    pub fn alloc_default<T: Default>(&self, len: usize) -> &mut [T] {
        let alloc = self.alloc_uninit(len);
        alloc.fill_with(|| MaybeUninit::new(T::default()));
        unsafe { alloc.assume_init_mut() }
    }

    pub fn alloc_uninit<T>(&self, len: usize) -> &mut [MaybeUninit<T>] {
        let ptr = self
            .alloc_raw(Layout::array::<T>(len).expect("bad layout"))
            .expect("OOM");
        unsafe { slice::from_raw_parts_mut(ptr.as_ptr() as _, len) }
    }

    #[cfg(feature = "virtual")]
    const COMMIT_CHUNK: usize = 1024 * 1024;
    #[cfg(feature = "virtual")]
    const PAGE_SIZE: usize = 1 << 16;

    pub fn alloc_raw(&self, layout: Layout) -> Result<NonNull<u8>, OOM> {
        let curr = unsafe { self.ptr.add(self.pos.get()) };
        let off = curr.align_offset(layout.align());

        let base = unsafe { curr.add(off) };
        self.pos.update(|p| p + off + layout.size());

        #[cfg(feature = "virtual")]
        if self.pos.get() > self.commited.get() {
            let to_reserve = (self.commited.get() + Self::COMMIT_CHUNK)
                .max(self.pos.get())
                .min(self.cap);
            let to_reserve = (to_reserve + Self::PAGE_SIZE - 1) & (Self::PAGE_SIZE - 1);
            unsafe {
                use core::slice::from_raw_parts_mut;
                virtual_mem::commit(from_raw_parts_mut(self.ptr, to_reserve))?
            };
        }

        if self.pos.get() > self.cap {
            self.pos.set(self.cap);
            Err(OOM)
        } else {
            Ok(unsafe { NonNull::new_unchecked(base) })
        }
    }
}

#[derive(Debug)]
pub struct OOM;

#[cfg(feature = "virtual")]
impl From<virtual_mem::Error> for OOM {
    fn from(_: virtual_mem::Error) -> Self {
        OOM
    }
}

#[cfg(feature = "virtual")]
impl Arna<'static> {
    pub fn new_virtual(cap: usize) -> Result<Self, virtual_mem::Error> {
        assert!(cap % Self::PAGE_SIZE == 0);
        let mem = virtual_mem::reserve(cap)?;
        Ok(Arna {
            ptr: mem as _,
            cap: mem.len(),
            commited: 0.into(),
            pos: 0.into(),
            depth: 0.into(),
            borrow: PhantomData,
            _unpin: PhantomPinned,
        })
    }
}

impl<'a> From<&'a mut [MaybeUninit<u8>]> for Arna<'a> {
    fn from(value: &'a mut [MaybeUninit<u8>]) -> Self {
        Arna {
            ptr: value.as_mut_ptr() as _,
            cap: value.len(),
            commited: Cell::new(value.len() + BackingMode::Slice as usize),
            pos: 0.into(),
            depth: 0.into(),
            borrow: PhantomData,
            _unpin: PhantomPinned,
        }
    }
}

#[cfg(feature = "alloc")]
impl From<alloc::boxed::Box<[MaybeUninit<u8>]>> for Arna<'static> {
    fn from(value: alloc::boxed::Box<[MaybeUninit<u8>]>) -> Self {
        let value = alloc::boxed::Box::leak(value);
        Arna {
            ptr: value.as_mut_ptr() as _,
            cap: value.len(),
            commited: Cell::new(value.len() + BackingMode::Box as usize),
            pos: 0.into(),
            depth: 0.into(),
            borrow: PhantomData,
            _unpin: PhantomPinned,
        }
    }
}

impl Drop for Arna<'_> {
    fn drop(&mut self) {
        assert!(self.depth.get() == 0, "all checkpoins need to be dropped");
        let _mem = slice_from_raw_parts_mut(self.ptr, self.cap);
        match self.backing_mode() {
            #[cfg(feature = "virtual")]
            BackingMode::Mmap => unsafe {
                virtual_mem::release(_mem as _).expect(
                    "the release has no reason to fail considering \
                    the invariants of the object",
                )
            },
            BackingMode::Slice => {}
            #[cfg(feature = "alloc")]
            BackingMode::Box => unsafe { drop(alloc::boxed::Box::from_raw(_mem)) },
        }
    }
}

#[cfg(feature = "virtual")]
pub mod virtual_mem {
    use core::ptr::slice_from_raw_parts_mut;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Error(i32);

    impl Error {
        pub const fn raw_os_error(self) -> i32 {
            self.0
        }
    }

    pub fn reserve(size: usize) -> Result<*mut [u8], Error> {
        if size == 0 {
            return Err(sys::invalid_parameter());
        }

        sys::reserve(size).map(|ptr| slice_from_raw_parts_mut(ptr, size))
    }

    /// Makes a reserved region readable and writable.
    ///
    /// # Safety
    ///
    /// `region` must be a currently reserved region returned by [`reserve`].
    pub unsafe fn commit(region: *mut [u8]) -> Result<(), Error> {
        let (ptr, len) = region_parts(region)?;
        unsafe { sys::commit(ptr, len) }
    }

    /// Discards a region's contents and makes it inaccessible while retaining its address range.
    ///
    /// # Safety
    ///
    /// `region` must be a currently reserved region returned by [`reserve`], and no references
    /// into it may be used until it is committed again.
    pub unsafe fn decommit(region: *mut [u8]) -> Result<(), Error> {
        let (ptr, len) = region_parts(region)?;
        unsafe { sys::decommit(ptr, len) }
    }

    /// Releases a reserved region back to the operating system.
    ///
    /// # Safety
    ///
    /// `region` must be a currently reserved region returned by [`reserve`], and it must not be
    /// used after this call.
    pub unsafe fn release(region: *mut [u8]) -> Result<(), Error> {
        let (ptr, len) = region_parts(region)?;
        unsafe { sys::release(ptr, len) }
    }

    fn region_parts(region: *mut [u8]) -> Result<(*mut u8, usize), Error> {
        let len = region.len();
        let ptr = region.cast::<u8>();
        if ptr.is_null() || len == 0 {
            Err(sys::invalid_parameter())
        } else {
            Ok((ptr, len))
        }
    }

    #[cfg(target_os = "linux")]
    mod sys {
        use super::Error;
        use core::ffi::{c_int, c_long};

        const PROT_NONE: usize = 0;
        const PROT_READ_WRITE: usize = 1 | 2;
        const MAP_PRIVATE_ANONYMOUS: usize = 2 | 0x20;
        const MADV_DONTNEED: usize = 4;

        #[cfg(target_arch = "x86_64")]
        const SYS_MMAP: c_long = 9;
        #[cfg(target_arch = "x86_64")]
        const SYS_MPROTECT: c_long = 10;
        #[cfg(target_arch = "x86_64")]
        const SYS_MUNMAP: c_long = 11;
        #[cfg(target_arch = "x86_64")]
        const SYS_MADVISE: c_long = 28;

        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        const SYS_MMAP: c_long = 222;
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        const SYS_MPROTECT: c_long = 226;
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        const SYS_MUNMAP: c_long = 215;
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        const SYS_MADVISE: c_long = 233;

        #[cfg(any(target_arch = "x86", target_arch = "arm"))]
        const SYS_MMAP: c_long = 192; // mmap2; a zero offset is identical to mmap.
        #[cfg(any(target_arch = "x86", target_arch = "arm"))]
        const SYS_MPROTECT: c_long = 125;
        #[cfg(any(target_arch = "x86", target_arch = "arm"))]
        const SYS_MUNMAP: c_long = 91;
        #[cfg(target_arch = "x86")]
        const SYS_MADVISE: c_long = 219;
        #[cfg(target_arch = "arm")]
        const SYS_MADVISE: c_long = 220;

        unsafe extern "C" {
            fn syscall(number: c_long, ...) -> c_long;
            fn __errno_location() -> *mut c_int;
        }

        pub(super) const fn invalid_parameter() -> Error {
            Error(22) // EINVAL
        }

        pub(super) fn reserve(size: usize) -> Result<*mut u8, Error> {
            let result = unsafe {
                syscall(
                    SYS_MMAP,
                    0usize,
                    size,
                    PROT_NONE,
                    MAP_PRIVATE_ANONYMOUS,
                    usize::MAX,
                    0usize,
                )
            };
            if result == -1 {
                Err(last_error())
            } else {
                Ok(result as usize as *mut u8)
            }
        }

        pub(super) unsafe fn commit(ptr: *mut u8, len: usize) -> Result<(), Error> {
            syscall_result(unsafe { syscall(SYS_MPROTECT, ptr as usize, len, PROT_READ_WRITE) })
        }

        pub(super) unsafe fn decommit(ptr: *mut u8, len: usize) -> Result<(), Error> {
            syscall_result(unsafe { syscall(SYS_MPROTECT, ptr as usize, len, PROT_NONE) })?;
            syscall_result(unsafe { syscall(SYS_MADVISE, ptr as usize, len, MADV_DONTNEED) })
        }

        pub(super) unsafe fn release(ptr: *mut u8, len: usize) -> Result<(), Error> {
            syscall_result(unsafe { syscall(SYS_MUNMAP, ptr as usize, len) })
        }

        fn syscall_result(result: c_long) -> Result<(), Error> {
            if result == -1 {
                Err(last_error())
            } else {
                Ok(())
            }
        }

        fn last_error() -> Error {
            Error(unsafe { *__errno_location() })
        }
    }

    #[cfg(target_os = "windows")]
    mod sys {
        use super::Error;
        use core::ffi::c_void;

        const MEM_COMMIT: u32 = 0x1000;
        const MEM_RESERVE: u32 = 0x2000;
        const MEM_DECOMMIT: u32 = 0x4000;
        const MEM_RELEASE: u32 = 0x8000;
        const PAGE_NOACCESS: u32 = 0x01;
        const PAGE_READWRITE: u32 = 0x04;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn VirtualAlloc(
                address: *mut c_void,
                size: usize,
                allocation_type: u32,
                protect: u32,
            ) -> *mut c_void;
            fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
            fn GetLastError() -> u32;
        }

        pub(super) const fn invalid_parameter() -> Error {
            Error(87) // ERROR_INVALID_PARAMETER
        }

        pub(super) fn reserve(size: usize) -> Result<*mut u8, Error> {
            let ptr =
                unsafe { VirtualAlloc(core::ptr::null_mut(), size, MEM_RESERVE, PAGE_NOACCESS) };
            if ptr.is_null() {
                Err(last_error())
            } else {
                Ok(ptr.cast())
            }
        }

        pub(super) unsafe fn commit(ptr: *mut u8, len: usize) -> Result<(), Error> {
            let result = unsafe { VirtualAlloc(ptr.cast(), len, MEM_COMMIT, PAGE_READWRITE) };
            if result.is_null() {
                Err(last_error())
            } else {
                Ok(())
            }
        }

        pub(super) unsafe fn decommit(ptr: *mut u8, len: usize) -> Result<(), Error> {
            unsafe { virtual_free(ptr, len, MEM_DECOMMIT) }
        }

        pub(super) unsafe fn release(ptr: *mut u8, _len: usize) -> Result<(), Error> {
            unsafe { virtual_free(ptr, 0, MEM_RELEASE) }
        }

        unsafe fn virtual_free(ptr: *mut u8, len: usize, free_type: u32) -> Result<(), Error> {
            if unsafe { VirtualFree(ptr.cast(), len, free_type) } == 0 {
                Err(last_error())
            } else {
                Ok(())
            }
        }

        fn last_error() -> Error {
            Error(unsafe { GetLastError() } as i32)
        }
    }

    #[cfg(target_os = "macos")]
    mod sys {
        use super::Error;
        use core::ffi::{c_int, c_void};

        const PROT_NONE: c_int = 0;
        const PROT_READ_WRITE: c_int = 1 | 2;
        const MAP_PRIVATE_ANONYMOUS: c_int = 2 | 0x1000;
        const MADV_DONTNEED: c_int = 4;

        unsafe extern "C" {
            fn mmap(
                address: *mut c_void,
                len: usize,
                protection: c_int,
                flags: c_int,
                fd: c_int,
                offset: i64,
            ) -> *mut c_void;
            fn mprotect(address: *mut c_void, len: usize, protection: c_int) -> c_int;
            fn madvise(address: *mut c_void, len: usize, advice: c_int) -> c_int;
            fn munmap(address: *mut c_void, len: usize) -> c_int;
            fn __error() -> *mut c_int;
        }

        pub(super) const fn invalid_parameter() -> Error {
            Error(22) // EINVAL
        }

        pub(super) fn reserve(size: usize) -> Result<*mut u8, Error> {
            let ptr = unsafe {
                mmap(
                    core::ptr::null_mut(),
                    size,
                    PROT_NONE,
                    MAP_PRIVATE_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr as isize == -1 {
                Err(last_error())
            } else {
                Ok(ptr.cast())
            }
        }

        pub(super) unsafe fn commit(ptr: *mut u8, len: usize) -> Result<(), Error> {
            result(unsafe { mprotect(ptr.cast(), len, PROT_READ_WRITE) })
        }

        pub(super) unsafe fn decommit(ptr: *mut u8, len: usize) -> Result<(), Error> {
            result(unsafe { mprotect(ptr.cast(), len, PROT_NONE) })?;
            result(unsafe { madvise(ptr.cast(), len, MADV_DONTNEED) })
        }

        pub(super) unsafe fn release(ptr: *mut u8, len: usize) -> Result<(), Error> {
            result(unsafe { munmap(ptr.cast(), len) })
        }

        fn result(result: c_int) -> Result<(), Error> {
            if result == -1 {
                Err(last_error())
            } else {
                Ok(())
            }
        }

        fn last_error() -> Error {
            Error(unsafe { *__error() })
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    compile_error!("the virtual feature only supports Linux, Windows, and macOS");

    #[cfg(test)]
    mod tests {
        use super::{commit, decommit, release, reserve};

        #[test]
        fn virtual_memory_lifecycle() {
            let region = reserve(8192).unwrap();
            unsafe { commit(region).unwrap() };

            let ptr = region.cast::<u8>();
            unsafe {
                ptr.write(42);
                ptr.add(8191).write(24);
                assert_eq!(ptr.read(), 42);
                assert_eq!(ptr.add(8191).read(), 24);
            }

            unsafe {
                decommit(region).unwrap();
                commit(region).unwrap();
            }
            unsafe {
                ptr.write(7);
                assert_eq!(ptr.read(), 7);
            }
            unsafe { release(region).unwrap() };
        }

        #[test]
        fn rejects_empty_reservations() {
            assert!(reserve(0).is_err());
        }
    }
}
