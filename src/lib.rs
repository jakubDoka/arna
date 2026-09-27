#![cfg_attr(not(feature = "std"), no_std)]

use core::{
    alloc::{AllocError, Allocator, Layout},
    cell::Cell,
    marker::{PhantomData, PhantomPinned},
    mem::{MaybeUninit, transmute},
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::{
        NonNull, copy_nonoverlapping, drop_in_place, null,
        slice_from_raw_parts_mut,
    },
    slice,
};

//pub fn lex(root) {
//    let modules = Mods::new(); // locked
//    let queu = Queue::new();
//    queu.push(Task::new(root));
//    let queue_ref = threads::broadcast(&queu):
//    let mods_ref = threads::broadcast(&modules);
//
//    while let Some(task) = queue_ref.next() {
//        {
//            let lexed = lex_file(task);
//            for import in &mut lexed {
//                let (link, is_new) = mods_ref.get_or_insert_module(task.from, import.path)
//                    .unwrap().link();
//                import.link = link;
//                if is_new {
//                    queue_ref.push(Task::new(link));
//                }
//            }
//        }
//
//        task.mark_done();
//    }
//
//    thread::barier();
//}

#[cfg(feature = "std")]
pub mod dynlib;
#[cfg(feature = "std")]
pub mod imui;

#[macro_export]
macro_rules! aformat {
    ($a:expr, $($tt:tt)*) => {
        $a.format(format_args!($($tt)*))
    };
}

#[cfg(feature = "alloc")]
extern crate alloc;

pub struct Checkpoint<'a> {
    arna: NonNull<Arna<'a>>,
    prev_pos: usize,
    depth: usize,
}

impl<'a> Checkpoint<'a> {
    /// # PANICS
    ///
    /// When you allocate to the parent before dropping the return value.
    pub fn checkpoint(&self) -> Self {
        let arna = unsafe { self.get_inner() };
        unsafe { Pin::new_unchecked(arna).checkpoint_ref() }
    }

    /// # SAFETY
    ///
    /// Only safe if all allocated references are unreachable.
    pub unsafe fn reset(&self) {
        let arna = unsafe { self.get_inner() };
        arna.pos.set(self.prev_pos);
    }

    /// # SAFETY
    ///
    /// Not safe to keep this ref around while allocating from other checkpoints as the depht
    /// check is performed here, so on your own risk, but it does allow you to avoid the
    /// repeated checks
    pub unsafe fn get_inner(&self) -> &Arna<'a> {
        let arna = unsafe { self.arna.as_ref() };
        assert_eq!(self.depth, arna.depth.get());
        arna
    }

    pub unsafe fn can_deallocate(
        &self,
        ptr: NonNull<u8>,
        layout: Layout,
    ) -> bool {
        let s = unsafe { self.get_inner() };
        unsafe { s.ptr.add(s.pos.get()).sub(layout.size()) == ptr.as_ptr() }
    }

    pub fn format<'b>(&'b self, args: core::fmt::Arguments) -> &'b mut str {
        use core::fmt::Write;

        struct Writer<'a, 'b> {
            check: &'b Checkpoint<'a>,
        }

        impl core::fmt::Write for Writer<'_, '_> {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let slot = self
                    .check
                    .try_alloc_uninit::<u8>(s.len())
                    .map_err(|_| core::fmt::Error)?;
                unsafe {
                    copy_nonoverlapping(
                        s as *const _ as *const u8,
                        slot as *mut _ as *mut _,
                        s.len(),
                    );
                }
                Ok(())
            }
        }

        let start = unsafe { self.get_inner() }.pos.get();

        let mut wrt = Writer { check: self };
        wrt.write_fmt(args).expect("OOM");

        let data = unsafe { self.get_inner().ptr.add(start) };
        let len = unsafe { self.get_inner() }.pos.get() - start;
        unsafe {
            core::str::from_utf8_unchecked_mut(core::slice::from_raw_parts_mut(
                data, len,
            ))
        }
    }

    pub fn create<T>(&self, value: T) -> &mut T {
        self.create_uninit().write(value)
    }

    pub fn create_uninit<T>(&self) -> &mut MaybeUninit<T> {
        unsafe { self.alloc_uninit(1).get_unchecked_mut(0) }
    }

    pub fn alloc<T>(&self, value: &[T]) -> &mut [T] {
        let alloc = self.alloc_uninit(value.len());
        unsafe {
            copy_nonoverlapping(
                value.as_ptr(),
                alloc.as_mut_ptr() as _,
                value.len(),
            )
        };
        unsafe { alloc.assume_init_mut() }
    }

    pub fn alloc_default<T: Default>(&self, len: usize) -> &mut OwnedSlice<T> {
        let alloc = self.alloc_uninit(len);
        alloc.fill_with(|| MaybeUninit::new(T::default()));
        unsafe { OwnedSlice::from_slice(alloc.assume_init_mut()) }
    }

    pub fn alloc_uninit<T>(&self, len: usize) -> &mut [MaybeUninit<T>] {
        self.try_alloc_uninit(len).expect("OOM")
    }

    pub fn try_alloc_uninit<T>(
        &self,
        len: usize,
    ) -> Result<&mut [MaybeUninit<T>], AllocError> {
        let ptr = unsafe { self.get_inner() }
            .alloc_raw(Layout::array::<T>(len).expect("bad layout"))?;
        Ok(unsafe { slice::from_raw_parts_mut(ptr.as_ptr() as _, len) })
    }
}

pub fn panicking() -> bool {
    cfg_select! {
        feature = "std" => std::thread::panicking(),
        _ => false,
    }
}

impl Drop for Checkpoint<'_> {
    fn drop(&mut self) {
        if !panicking() {
            let arna = unsafe { self.get_inner() };
            arna.pos.set(self.prev_pos);
            arna.depth.update(|d| d - 1);
        }
    }
}

pub struct OwnedSlice<T>([T]);

impl<T> DerefMut for OwnedSlice<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> Deref for OwnedSlice<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> OwnedSlice<T> {
    pub unsafe fn from_slice(slice: &mut [T]) -> &mut OwnedSlice<T> {
        unsafe { transmute(slice) }
    }
}

impl<'a, T: Copy> Into<&'a mut [T]> for &'a mut OwnedSlice<T> {
    fn into(self) -> &'a mut [T] {
        return &mut self.0;
    }
}

impl<T> Drop for OwnedSlice<T> {
    fn drop(&mut self) {
        unsafe {
            drop_in_place(&mut self.0);
        }
    }
}

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackingMode {
    #[cfg(feature = "virtual")]
    Virtual = 0,
    Slice = 1,
    #[cfg(feature = "alloc")]
    Box = 2,
}

/// Arna is just the memory owner, to allocate use [`Self::checkpoint`]
#[derive(Default, Debug)]
pub struct Arna<'a> {
    pos: Cell<usize>,
    commited: Cell<usize>,
    ptr: *mut u8,
    cap: usize,
    depth: Cell<usize>,

    borrow: PhantomData<&'a mut [u8]>,
    _unpin: PhantomPinned,
}

#[cfg(feature = "std")]
pub type TempArenas =
    std::thread::LocalKey<core::cell::UnsafeCell<[Arna<'static>; 2]>>;

#[cfg(feature = "std")]
thread_local! {
    pub static TEMP_ARENAS: core::cell::UnsafeCell<[Arna<'static>; 2]> = Default::default();
}

impl<'a> Arna<'a> {
    pub fn backing_mode(&self) -> BackingMode {
        if self.cap == 0 {
            return BackingMode::Slice;
        }
        let vl = (self.commited.get() as isize - self.cap as isize).max(0);
        debug_assert!(vl < 3);
        unsafe { transmute(vl) }
    }

    #[cfg(feature = "std")]
    pub fn scratch(clobber: impl IntoClobber) -> Checkpoint<'static> {
        cfg_select! {
            feature = "dll" => {
                let ptr = ARNA_TEMP_ARENAS_OVERRIDE
                    .load(core::sync::atomic::Ordering::Relaxed);

                // SAFETY: the person initializing the DLL_TEMP_ARENAS passed a valid
                // pointer
                let temp = match unsafe { ptr.as_ref() } {
                    Some(v) => v,
                    None => &TEMP_ARENAS,
                };

                Self::scratch_with(clobber, temp)
            }
            _ => Self::scratch_with(clobber, &TEMP_ARENAS),
        }
    }

    #[cfg(feature = "std")]
    pub fn scratch_with(
        clobber: impl IntoClobber,
        temp: &'static TempArenas,
    ) -> Checkpoint<'static> {
        let clobber = clobber.address();
        temp.with(|arenas| {
            let refr = unsafe { &*arenas.get() };
            for vl in refr {
                if vl as *const _ as *const _ != clobber {
                    return unsafe { Pin::new_unchecked(vl).checkpoint_ref() };
                }
            }
            unreachable!()
        })
    }

    #[cfg(feature = "std")]
    pub fn init_temp_arenas(slots: [Arna<'static>; 2]) {
        // SAFETY: the drop of the previous arenas will panic if any checkpoints are still
        // active
        TEMP_ARENAS.with(|old_slots| unsafe { *old_slots.get() = slots })
    }

    #[cfg(feature = "virtual")]
    pub fn clear_and_decommit(&mut self) {
        // SAFETY: the checkpoints can only be created from the pinned arena, that means we
        // cant call this
        assert_eq!(self.backing_mode(), BackingMode::Virtual);
        unsafe {
            use core::slice::from_raw_parts_mut;

            virtual_mem::decommit(from_raw_parts_mut(
                self.ptr,
                self.commited.get(),
            ))
            .expect("this should not fail considering the invariants")
        };
        self.commited.set(0);
        self.pos.set(0);
    }

    pub fn checkpoint(self: Pin<&mut Self>) -> Checkpoint<'a> {
        self.as_ref().checkpoint_ref()
    }

    pub fn checkpoint_ref(self: Pin<&Self>) -> Checkpoint<'a> {
        self.depth.update(|d| d + 1);
        Checkpoint {
            arna: self.get_ref().into(),
            depth: self.depth.get(),
            prev_pos: self.pos.get(),
        }
    }

    pub fn alloc_raw(
        &self,
        layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let curr = unsafe { self.ptr.add(self.pos.get()) };
        let off = curr.align_offset(layout.align());

        let base = unsafe { curr.add(off) };

        if self.pos.get() + layout.size() > self.cap {
            return Err(AllocError);
        }

        self.pos.update(|p| p + off + layout.size());

        #[cfg(feature = "virtual")]
        if self.pos.get() > self.commited.get() {
            let to_reserve = (self.commited.get() + Arna::COMMIT_CHUNK)
                .max(self.pos.get())
                .min(self.cap);
            let to_reserve =
                (to_reserve + Arna::PAGE_SIZE - 1) & !(Arna::PAGE_SIZE - 1);
            assert!(to_reserve != 0);
            unsafe {
                virtual_mem::commit(slice_from_raw_parts_mut(
                    self.ptr.add(self.commited.get()),
                    to_reserve - self.commited.get(),
                ))?
            };
            self.commited.set(to_reserve);
        }

        Ok(unsafe {
            let slc = slice_from_raw_parts_mut(base, layout.size());
            NonNull::new_unchecked(slc)
        })
    }
}

pub trait IntoClobber {
    fn address(self) -> *const ();
}

impl IntoClobber for i32 {
    fn address(self) -> *const () {
        null()
    }
}

impl IntoClobber for &'_ Checkpoint<'_> {
    fn address(self) -> *const () {
        self.arna.as_ptr() as _
    }
}

#[cfg(feature = "dll")]
#[unsafe(no_mangle)]
pub static ARNA_TEMP_ARENAS_OVERRIDE: core::sync::atomic::AtomicPtr<
    TempArenas,
> = core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

#[cfg(feature = "virtual")]
impl Arna<'static> {
    const COMMIT_CHUNK: usize = 1024 * 1024;
    // NOTE: shoud cover all platforms, eventhough overdoing it on some
    const PAGE_SIZE: usize = 1 << 16;

    pub fn new_virtual(cap: usize) -> Result<Self, virtual_mem::Error> {
        assert!(cap % Self::PAGE_SIZE == 0);
        let mem = virtual_mem::reserve(cap)?;
        Ok(unsafe { Arna::new(mem as _, BackingMode::Virtual) })
    }

    pub fn init_bulk<const COUNT: usize>(
        caps: [usize; COUNT],
    ) -> Result<[Arna<'static>; COUNT], virtual_mem::Error> {
        let mut arnas: [MaybeUninit<Arna<'static>>; COUNT] =
            [const { MaybeUninit::uninit() }; COUNT];

        assert!(caps.iter().all(|v| v % Self::PAGE_SIZE == 0));
        cfg_select! {
            target_os = "windows" => {
                // Windows sucks in this regard, it does not let us free arbitrary ranges of
                // the allocation
                for (arna, cap) in arnas.iter_mut().zip(caps) {
                    arna.write(Arna::new_virtual(cap)?);
                }
            }
            _ => {
                let total_cap = caps.iter().sum::<usize>();
                let mut mem = virtual_mem::reserve(total_cap)?;

                for (arna, cap) in arnas.iter_mut().zip(caps) {
                    use core::ptr::slice_from_raw_parts_mut;

                    arna.write(unsafe {
                        Arna::new(
                            slice_from_raw_parts_mut(mem as _, cap),
                            BackingMode::Virtual,
                        )
                    });

                    mem = unsafe {
                        slice_from_raw_parts_mut(
                            (mem as *mut u8).add(cap),
                            mem.len() - cap,
                        )
                    };
                }
            }
        }

        Ok(unsafe { arnas.map(|v| v.assume_init()) })
    }
}

impl<'a> From<&'a mut [u8]> for Arna<'a> {
    fn from(value: &'a mut [u8]) -> Self {
        unsafe { Arna::new(value as *mut _ as *mut _, BackingMode::Slice) }
    }
}

impl<'a> From<&'a mut [MaybeUninit<u8>]> for Arna<'a> {
    fn from(value: &'a mut [MaybeUninit<u8>]) -> Self {
        unsafe { Arna::new(value, BackingMode::Slice) }
    }
}

#[cfg(feature = "alloc")]
impl From<alloc::boxed::Box<[MaybeUninit<u8>]>> for Arna<'static> {
    fn from(value: alloc::boxed::Box<[MaybeUninit<u8>]>) -> Self {
        unsafe { Arna::new(alloc::boxed::Box::leak(value), BackingMode::Slice) }
    }
}

impl Arna<'static> {
    /// If you are unsure how to use this, use `new_virtual` or From imps
    pub unsafe fn new(mem: *mut [MaybeUninit<u8>], mode: BackingMode) -> Self {
        Arna {
            ptr: mem as _,
            cap: mem.len(),
            commited: Cell::new(match mode {
                #[cfg(feature = "virtual")]
                BackingMode::Virtual => 0,
                _ => mem.len() + mode as usize,
            }),
            pos: 0.into(),
            depth: 0.into(),
            borrow: PhantomData,
            _unpin: PhantomPinned,
        }
    }
}

impl Drop for Arna<'_> {
    fn drop(&mut self) {
        assert!(
            panicking() || self.depth.get() == 0,
            "all checkpoins need to be dropped"
        );
        let _mem = slice_from_raw_parts_mut(self.ptr, self.cap);
        match self.backing_mode() {
            #[cfg(feature = "virtual")]
            BackingMode::Virtual => unsafe {
                virtual_mem::release(_mem as _).expect(
                    "the release has no reason to fail considering \
                    the invariants of the object",
                )
            },
            BackingMode::Slice => {}
            #[cfg(feature = "alloc")]
            BackingMode::Box => unsafe {
                drop(alloc::boxed::Box::from_raw(_mem))
            },
        }
    }
}

unsafe impl<'a> Allocator for Checkpoint<'a> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.get_inner() }.alloc_raw(layout).map_err(|_| AllocError)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if unsafe { self.can_deallocate(ptr, layout) } {
            unsafe { self.get_inner() }.pos.update(|p| p - layout.size());
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let can_reuse = unsafe { self.can_deallocate(ptr, old_layout) };
        let can_reuse = can_reuse && new_layout.align() == old_layout.align();
        let s = unsafe { self.get_inner() };

        if can_reuse {
            s.pos.update(|p| p - old_layout.size());
        }

        let new = s.alloc_raw(new_layout)?;

        if !can_reuse {
            unsafe {
                copy_nonoverlapping(
                    ptr.as_ptr(),
                    new.as_ptr() as *mut _,
                    old_layout.size(),
                );
            }
        }

        Ok(new)
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let new = unsafe { self.grow(ptr, old_layout, new_layout) }?;
        unsafe {
            (new.as_ptr() as *mut u8)
                .add(old_layout.size())
                .write_bytes(0, new_layout.size() - old_layout.size())
        }
        Ok(new)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, core::alloc::AllocError> {
        debug_assert!(
            new_layout.size() <= old_layout.size(),
            "`new_layout.size()` must be smaller than or equal to `old_layout.size()`"
        );

        if unsafe { self.can_deallocate(ptr, old_layout) } {
            unsafe { self.get_inner() }
                .pos
                .update(|p| p - (old_layout.size() - new_layout.size()));
        }

        let slc = slice_from_raw_parts_mut(ptr.as_ptr(), new_layout.size());
        Ok(unsafe { NonNull::new_unchecked(slc) })
    }
}

#[cfg(test)]
pub mod tests {
    use {crate::Arna, core::pin::pin};

    #[cfg(feature = "alloc")]
    #[test]
    fn temp_arenas() {
        use core::mem::MaybeUninit;

        Arna::init_temp_arenas([
            Arna::from(Box::from_iter([MaybeUninit::<u8>::uninit(); 1024])),
            Arna::from(Box::from_iter([MaybeUninit::<u8>::uninit(); 1024])),
        ]);

        let check = Arna::scratch(0);

        let vl = check.alloc_default::<u8>(16);

        let check_2 = Arna::scratch(&check);
        let mem = {
            let _check_3 = Arna::scratch(&check_2);

            let mem = _check_3.alloc_default::<u8>(16);

            let mem = check_2.alloc(mem);
            mem.fill(10);

            assert_eq!(aformat!(check_2, "foob {}", 10), "foob 10");

            mem
        };

        check.alloc_default::<u8>(16).fill(40);
        vl.fill(10);
        mem.fill(11);
    }

    #[test]
    #[should_panic]
    fn invariat_crash_() {
        let mut buf = [0; 1024];
        let check = {
            let mut arena = pin!(Arna::from(&mut buf[..]));

            let check = arena.as_mut().checkpoint();

            _ = check.alloc_default::<usize>(16);

            arena.checkpoint()
        };

        _ = check.alloc_default::<u8>(16);
    }

    #[test]
    #[should_panic]
    fn invariat_crash() {
        let mut buf = [0; 1024];

        let arena = pin!(Arna::from(&mut buf[..]));

        let check = arena.checkpoint();

        _ = check.alloc_default::<u8>(16);

        let _check_2 = check.checkpoint();

        _ = check.alloc_default::<u8>(16);
    }

    #[test]
    fn virtual_meme() {
        Arna::init_temp_arenas(
            Arna::init_bulk([1024 * 1024 * 8; 2]).expect("brahm"),
        );

        let check = Arna::scratch(0);

        let mem = check.alloc_default::<u8>(1024 * 1024 + 1);
        mem.fill(1);

        let check2 = Arna::scratch(&check);
        let mem = check2.alloc_default::<u8>(1024 * 1024 + 1);
        mem.fill(1);
    }
}

#[cfg(feature = "virtual")]
pub mod virtual_mem {
    use core::{alloc::AllocError, ptr::slice_from_raw_parts_mut};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Error(i32);

    impl From<Error> for AllocError {
        fn from(_: Error) -> Self {
            AllocError
        }
    }

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
        use {
            super::Error,
            core::ffi::{c_int, c_long},
        };

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

        pub(super) unsafe fn commit(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            syscall_result(unsafe {
                syscall(SYS_MPROTECT, ptr as usize, len, PROT_READ_WRITE)
            })
        }

        pub(super) unsafe fn decommit(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            syscall_result(unsafe {
                syscall(SYS_MPROTECT, ptr as usize, len, PROT_NONE)
            })?;
            syscall_result(unsafe {
                syscall(SYS_MADVISE, ptr as usize, len, MADV_DONTNEED)
            })
        }

        pub(super) unsafe fn release(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            syscall_result(unsafe { syscall(SYS_MUNMAP, ptr as usize, len) })
        }

        fn syscall_result(result: c_long) -> Result<(), Error> {
            if result == -1 { Err(last_error()) } else { Ok(()) }
        }

        fn last_error() -> Error {
            Error(unsafe { *__errno_location() })
        }
    }

    #[cfg(target_os = "windows")]
    mod sys {
        use {super::Error, core::ffi::c_void};

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
            fn VirtualFree(
                address: *mut c_void,
                size: usize,
                free_type: u32,
            ) -> i32;
            fn GetLastError() -> u32;
        }

        pub(super) const fn invalid_parameter() -> Error {
            Error(87) // ERROR_INVALID_PARAMETER
        }

        pub(super) fn reserve(size: usize) -> Result<*mut u8, Error> {
            let ptr = unsafe {
                VirtualAlloc(
                    core::ptr::null_mut(),
                    size,
                    MEM_RESERVE,
                    PAGE_NOACCESS,
                )
            };
            if ptr.is_null() { Err(last_error()) } else { Ok(ptr.cast()) }
        }

        pub(super) unsafe fn commit(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            let result = unsafe {
                VirtualAlloc(ptr.cast(), len, MEM_COMMIT, PAGE_READWRITE)
            };
            if result.is_null() { Err(last_error()) } else { Ok(()) }
        }

        pub(super) unsafe fn decommit(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            unsafe { virtual_free(ptr, len, MEM_DECOMMIT) }
        }

        pub(super) unsafe fn release(
            ptr: *mut u8,
            _len: usize,
        ) -> Result<(), Error> {
            unsafe { virtual_free(ptr, 0, MEM_RELEASE) }
        }

        unsafe fn virtual_free(
            ptr: *mut u8,
            len: usize,
            free_type: u32,
        ) -> Result<(), Error> {
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
        use {
            super::Error,
            core::ffi::{c_int, c_void},
        };

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
            fn mprotect(
                address: *mut c_void,
                len: usize,
                protection: c_int,
            ) -> c_int;
            fn madvise(
                address: *mut c_void,
                len: usize,
                advice: c_int,
            ) -> c_int;
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
            if ptr as isize == -1 { Err(last_error()) } else { Ok(ptr.cast()) }
        }

        pub(super) unsafe fn commit(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            result(unsafe { mprotect(ptr.cast(), len, PROT_READ_WRITE) })
        }

        pub(super) unsafe fn decommit(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            result(unsafe { mprotect(ptr.cast(), len, PROT_NONE) })?;
            result(unsafe { madvise(ptr.cast(), len, MADV_DONTNEED) })
        }

        pub(super) unsafe fn release(
            ptr: *mut u8,
            len: usize,
        ) -> Result<(), Error> {
            result(unsafe { munmap(ptr.cast(), len) })
        }

        fn result(result: c_int) -> Result<(), Error> {
            if result == -1 { Err(last_error()) } else { Ok(()) }
        }

        fn last_error() -> Error {
            Error(unsafe { *__error() })
        }
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "windows",
        target_os = "macos"
    )))]
    compile_error!(
        "the virtual feature only supports Linux, Windows, and macOS"
    );

    #[cfg(test)]
    #[cfg(not(miri))]
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
