use {
    crate::{Arna, Checkpoint, OwnedSlice},
    alloc::sync::Arc,
    barrier::Barrier,
    core::{
        any::{Any, TypeId},
        cell::{Cell, RefCell, UnsafeCell},
        ops::{Deref, Range},
    },
};

pub mod barrier;

pub struct BroadcastPtr {
    ptr: *const (),
    id: TypeId,
}

pub type Group = usize;

impl BroadcastPtr {
    pub unsafe fn downcast<T: Any + ?Sized, C: Copy>(&self) -> Option<C> {
        if self.id == TypeId::of::<T>() {
            Some(unsafe { *self.ptr.cast::<C>() })
        } else {
            None
        }
    }
}

impl Default for BroadcastPtr {
    fn default() -> Self {
        Self { ptr: Default::default(), id: TypeId::of::<()>() }
    }
}

pub struct GlobalState {
    broadcast_ptrs: [UnsafeCell<BroadcastPtr>; 3],
    share_barier: Barrier,
    count: usize,
}

impl GlobalState {
    pub fn new(count: usize) -> Self {
        Self {
            broadcast_ptrs: Default::default(),
            share_barier: Barrier::new(count),
            count,
        }
    }
}

unsafe impl Send for GlobalState {}
unsafe impl Sync for GlobalState {}

pub struct LocalState {
    global: Arc<GlobalState>,
    index: usize,
    depth: usize,
}

thread_local! {
    static LOCAL_STATE: RefCell<Option<LocalState>> = RefCell::new(None);
}

pub fn launch(lane_count: usize, main: impl Fn() + Sync + Send) {
    let main = &main;
    let global_state = Arc::new(GlobalState::new(lane_count));
    std::thread::scope(|t| {
        for lane_index in 0..lane_count {
            let global = global_state.clone();
            std::thread::Builder::new()
                .name(format!("lane-{lane_index}"))
                .spawn_scoped(t, move || {
                    LOCAL_STATE.with(|s| {
                        *s.borrow_mut() = Some(LocalState {
                            global,
                            index: lane_index,
                            depth: 0,
                        });
                    });

                    main();
                })
                .expect("wut");
        }
    })
}

fn with_local<T>(
    depth: impl Into<Option<usize>>,
    with: impl FnOnce(&mut LocalState) -> T,
) -> T {
    LOCAL_STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut().expect("not called withing the lane thread");
        if let Some(depth) = depth.into() {
            assert_eq!(depth, s.depth);
        }
        with(s)
    })
}

pub struct Erased {
    ptr: *mut (),
    drop_impl: unsafe fn(*mut ()),
}

impl Default for Erased {
    fn default() -> Self {
        Self { ptr: Default::default(), drop_impl: |_| {} }
    }
}

impl Drop for Erased {
    fn drop(&mut self) {
        unsafe { (self.drop_impl)(self.ptr) }
    }
}

pub const BARRIER_PURPOSE_BATCH: u64 = 0;
pub const BARRIER_PURPOSE_DROP: u64 = 1;
pub const BARRIER_PURPOSE_SYNC: u64 = 2;

pub fn usage(depht: usize, purpose: u64) -> u64 {
    purpose | (depht as u64) << 2
}

pub struct ScopeMemory<'b> {
    to_drop: Cell<Erased>,
    checkpoint: Checkpoint<'b>,
    depth: usize,
}

impl<'b> ScopeMemory<'b> {
    /// # Safety
    ///
    /// Caller must guarantee the `checkpoint` is dropped after `to_drop`
    pub fn batch<
        'a,
        PC: FnMut() -> &'a mut [P],
        P: Send + Sync + Any,
        SC: FnOnce() -> &'a S,
        S: Send + Sync + Any + ?Sized,
        B: Send + Sync + Any,
    >(
        &'a self,
        mut partition: PC,
        state: SC,
        broadcast: B,
    ) -> (&'a mut [P], &'a S, &'a [B]) {
        if true {
            panic!("this is useless and broken");
        }

        with_local(self.depth, |local| {
            let part;
            let state_ptr;
            let broad;
            if local.index == 0 {
                part = partition();
                unsafe {
                    *local.global.broadcast_ptrs[0].get() = BroadcastPtr {
                        ptr: &part as *const _ as *const _,
                        id: TypeId::of::<P>(),
                    }
                }

                state_ptr = state();

                unsafe {
                    *local.global.broadcast_ptrs[1].get() = BroadcastPtr {
                        ptr: &state_ptr as *const _ as *const _,
                        id: TypeId::of::<S>(),
                    };
                }

                broad = self.checkpoint.alloc_uninit::<B>(local.global.count);
                unsafe {
                    *local.global.broadcast_ptrs[2].get() = BroadcastPtr {
                        ptr: &broad as *const _ as *const _,
                        id: TypeId::of::<B>(),
                    };
                }
            }

            local
                .global
                .share_barier
                .wait(usage(self.depth, BARRIER_PURPOSE_BATCH));

            let part = unsafe {
                (*local.global.broadcast_ptrs[0].get())
                    .downcast::<P, *mut [P]>()
                    .ok_or("partition")
            };

            let state = unsafe {
                (*local.global.broadcast_ptrs[1].get())
                    .downcast::<S, &S>()
                    .ok_or("state")
            };

            let broad = unsafe {
                (*local.global.broadcast_ptrs[2].get())
                    .downcast::<B, *mut [B]>()
                    .ok_or("broadcast")
            };

            let (part, state, broad) = (part?, state?, broad?);

            unsafe { (broad as *mut B).add(local.index).write(broadcast) };

            local
                .global
                .share_barier
                .wait(usage(self.depth, BARRIER_PURPOSE_BATCH));

            if local.index == 0 {
                if core::mem::needs_drop::<B>() {
                    self.create_shared(Dropper(broad));
                }
            }

            let range =
                task_slice_bounds(part.len(), local.global.count, local.index);

            Ok::<_, &'static str>((
                unsafe {
                    core::slice::from_raw_parts_mut(
                        (part as *mut P).add(range.start),
                        range.len(),
                    )
                },
                state,
                unsafe { &*broad },
            ))
        })
        .expect("type mismatch")
    }

    pub fn create_shared_slice<'a, T: 'static>(
        &'a self,
        s: OwnedSlice<'a, T>,
    ) -> &'a mut [T] {
        let slice = s.leak();

        self.create_shared(Dropper(slice as *mut _));

        slice
    }

    pub fn create_shared<'a, S: 'static>(&'a self, s: S) -> &'a mut S {
        if core::mem::needs_drop::<S>() {
            let state = self
                .checkpoint
                .create_uninit()
                .write(EraseNode { s, _next: self.to_drop.take() });
            let ptr = state as *mut _ as *mut _;
            self.to_drop.set(Erased {
                ptr,
                drop_impl: unsafe {
                    core::mem::transmute(
                        core::ptr::drop_in_place::<EraseNode<S>>
                            as unsafe fn(*mut EraseNode<S>),
                    )
                },
            });
            unsafe { &mut (*ptr.cast::<EraseNode<S>>()).s }
        } else {
            self.checkpoint.create(s)
        }
    }

    pub fn broadcast<'a, I: Send + Sync + Any>(&self, input: I) -> &[I] {
        self.batch(|| -> &mut [()] { &mut [] }, || &(), input).2
    }

    pub fn share<'a, S: Send + Sync + Any>(&'a self, state: S) -> &'a S {
        self.share_with(|| self.create_shared(state))
    }

    pub fn share_with<
        'a,
        SC: FnOnce() -> &'a S,
        S: Send + Sync + Any + ?Sized,
    >(
        &'a self,
        state: SC,
    ) -> &'a S {
        self.batch(|| -> &mut [()] { &mut [] }, state, || {}).1
    }

    pub fn partition<
        'a,
        PC: FnMut() -> &'a mut [P],
        P: Send + Sync + 'static,
    >(
        &'a self,
        partition: PC,
    ) -> &'a mut [P] {
        self.batch(partition, || &(), || {}).0
    }
}

struct Dropper<B>(*mut [B]);

impl<B> Drop for Dropper<B> {
    fn drop(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self.0);
        }
    }
}

unsafe impl<B: Send> Send for Dropper<B> {}
unsafe impl<B: Sync> Sync for Dropper<B> {}

impl<'b> Deref for ScopeMemory<'b> {
    type Target = Checkpoint<'b>;

    fn deref(&self) -> &Self::Target {
        &self.checkpoint
    }
}

pub struct Scope<'b> {
    mem: ScopeMemory<'b>,
    prev_local: Option<LocalState>,
    pub group: usize,
}

impl<'b> Deref for Scope<'b> {
    type Target = ScopeMemory<'b>;

    fn deref(&self) -> &Self::Target {
        &self.mem
    }
}

pub struct EraseNode<S> {
    s: S,
    _next: Erased,
}

pub fn task_slice_bounds(
    values_count: usize,
    thread_count: usize,
    thread_idx: usize,
) -> Range<usize> {
    let values_per_thread = values_count / thread_count;
    let leftover_values_count = values_count % thread_count;
    let thread_has_leftover = thread_idx < leftover_values_count;
    let leftovers_before_this_thread_idx =
        if thread_has_leftover { thread_idx } else { leftover_values_count };
    let thread_first_value_idx =
        values_per_thread * thread_idx + leftovers_before_this_thread_idx;
    let thread_opl_value_idx = thread_first_value_idx
        + values_per_thread
        + thread_has_leftover as usize;
    thread_first_value_idx..thread_opl_value_idx
}

pub fn index() -> usize {
    with_local(None, |l| l.index)
}

pub fn count() -> usize {
    with_local(None, |l| l.global.count)
}

pub fn scope(checkpoint: Checkpoint) -> Scope {
    scope_with_group_projection(checkpoint, None)
}

pub fn scope_group_with(
    checkpoint: Checkpoint,
    mut partitioner: impl FnMut(usize) -> usize,
) -> Scope {
    let scratch = Arna::scratch(&checkpoint);
    let mut i = 0;
    let projection = scratch.alloc_with(count(), || {
        i += 1;
        partitioner(i - 1)
    });
    scope_with_group_projection(checkpoint, Some(&projection))
}

pub fn scope_with_group_projection<'a>(
    checkpoint: Checkpoint<'a>,
    group_projection: Option<&[usize]>,
) -> Scope<'a> {
    unsafe fn perform<'a, 'b>(
        mem: &'a ScopeMemory<'b>,
        groups: &[usize],
    ) -> (usize, LocalState) {
        let index = index();
        let count = count();

        assert_eq!(groups.len(), count);

        let current = groups[index];
        let index = groups[..index].iter().filter(|&&v| v == current).count();

        let (_, shared, broad) = mem.batch(
            || -> &mut [()] { &mut [] },
            move || {
                let group_count = groups
                    .iter()
                    .copied()
                    .max()
                    .expect("we absolutely have at least one lane")
                    + 1;

                let thread_counts =
                    mem.alloc_default::<u8>(group_count as usize).unwrap();
                for &group in groups {
                    thread_counts[group as usize] += 1;
                }

                let mut elems = thread_counts
                    .iter()
                    .map(|&count| Arc::new(GlobalState::new(count as usize)));

                let slots: OwnedSlice<'a, _> = mem
                    .alloc_with(thread_counts.len(), || {
                        elems.next().expect("we have the same length")
                    });

                mem.create_shared_slice(slots)
            },
            {
                // SAFETY: we dont let this escape past the lifetime of &'a self and so the
                // reference allocated on the selfs arena should be valid in the following code.
                struct Smuggle(*const [usize]);

                unsafe impl Send for Smuggle {}
                unsafe impl Sync for Smuggle {}

                Smuggle(mem.alloc(groups))
            },
        );

        let global = shared[current as usize].clone();

        if !broad[1..].iter().all(|v| unsafe { *v.0 == *broad[0].0 }) {
            with_local(mem.depth, |l| {
                l.global
                    .share_barier
                    .wait(usage(mem.depth, BARRIER_PURPOSE_BATCH))
            });
            panic!("projection mismatch");
        }

        (
            current,
            with_local(mem.depth, |l| {
                core::mem::replace(
                    l,
                    LocalState { global, index, depth: l.depth },
                )
            }),
        )
    }

    let mem = ScopeMemory {
        to_drop: Default::default(),
        checkpoint,
        depth: with_local(None, |l| {
            l.depth += 1;
            l.depth
        }),
    };
    let mut prev_local = None;
    let mut group = 0;
    if let Some(group_projection) = group_projection {
        let (new_group, new_prev_local) =
            unsafe { perform(&mem, group_projection) };
        group = new_group;
        prev_local = Some(new_prev_local);
    }

    Scope { mem, prev_local, group }
}

impl<'b> Scope<'b> {
    pub fn sync(&self) {
        with_local(self.depth, |l| {
            l.global.share_barier.wait(usage(self.depth, BARRIER_PURPOSE_SYNC))
        });
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if let Some(prev_local) = &mut self.prev_local {
            with_local(self.mem.depth, |l| core::mem::swap(l, prev_local));
        }

        // NOTE: we sync and then drop, this means that if user did not drop any of the scopes for
        // some reason we deadlock, that makes this api sound, it also means the `mem` will remain
        // valid, as far as I can tell there should not be any undefined behaviour
        with_local(self.depth, |l| {
            l.depth -= 1;
            self.prev_local
                .as_ref()
                .map_or(&l.global, |l| &l.global)
                .share_barier
                .wait(usage(self.depth, BARRIER_PURPOSE_DROP))
        });
    }
}

#[cfg(test)]
mod test {
    use {
        crate::{Arna, lane},
        core::sync::atomic::{AtomicBool, AtomicU16, Ordering},
    };

    #[test]
    pub fn sanity() {
        lane::launch(16, || {
            Arna::init_temp_arenas_with_boxes(1024 * 8);

            {
                let scope = lane::scope(Arna::scratch(0));

                let slice =
                    scope.partition(|| scope.alloc_with(1024, || 1).unwrap());

                let sum = slice.iter().sum::<u16>();

                let accum2 = scope.share(AtomicU16::new(0));

                accum2.fetch_add(sum, Ordering::Relaxed);

                let accum1 = scope.broadcast(sum);

                assert_eq!(accum1.iter().sum::<u16>(), 1024);
                assert_eq!(accum2.load(Ordering::Relaxed), 1024);

                {
                    enum GroupKind {
                        Eaven(u16),
                        Odd(u16),
                    }

                    let scratch = Arna::scratch(&**scope);
                    let split_scope =
                        lane::scope_group_with(scratch, |i| i % 2);

                    let slice = split_scope.partition(|| {
                        split_scope.alloc_with(1024, || 1).unwrap()
                    });

                    let sum = slice.iter().sum::<u16>();

                    let kind = match split_scope.group == 0 {
                        true => GroupKind::Eaven(sum),
                        false => GroupKind::Odd(sum),
                    };

                    drop(split_scope);

                    let slots = scope.broadcast(kind);

                    let [mut eaven, mut odd] = [0; 2];

                    for slot in slots {
                        match slot {
                            GroupKind::Eaven(e) => eaven += e,
                            GroupKind::Odd(o) => odd += o,
                        }
                    }

                    assert_eq!(eaven, 1024);
                    assert_eq!(odd, 1024);
                }
            }
        });
    }

    /// `create_shared` moves the existing drop chain into the value passed to
    /// the arena before allocating space for it. If that allocation panics,
    /// unwinding drops the chain while references to its values remain live.
    #[ignore = "demonstrates UB (use-after-free): cargo miri test --lib lane::test::unsound_create_shared_oom_uaf -- --ignored --exact"]
    #[test]
    fn unsound_create_shared_oom_uaf() {
        let node_size = core::mem::size_of::<lane::EraseNode<String>>();
        let node_align = core::mem::align_of::<lane::EraseNode<String>>();

        lane::launch(1, || {
            // The first node fits regardless of the backing allocation's
            // alignment, but there is not enough room for a second node.
            Arna::init_temp_arenas_with_boxes(node_size + node_align - 1);
            let scope = lane::scope(Arna::scratch(0));
            let shared = scope.create_shared(String::from("still live"));

            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    scope.create_shared(String::from("forces OOM"));
                }));
            assert!(result.is_err());

            // Safe Rust still considers `shared` live, but unwinding the
            // failed allocation has already destroyed its String.
            std::hint::black_box(shared.as_bytes()[0]);
        });
    }

    /// Values created earlier are dropped after values created later. Safe

    /// interior mutability can make an earlier destructor borrow a later value,
    /// so the destructor observes that value after it has been destroyed.
    #[cfg(false)]
    #[ignore = "demonstrates UB (use-after-free): cargo miri test --lib lane::test::unsound_create_shared_drop_order_uaf -- --ignored --exact"]
    #[test]
    fn unsound_create_shared_drop_order_uaf() {
        struct Observer<'a>(std::sync::Mutex<Option<&'a String>>);

        impl Drop for Observer<'_> {
            fn drop(&mut self) {
                std::hint::black_box(
                    self.0.lock().unwrap().unwrap().as_bytes()[0],
                );
            }
        }

        lane::launch(1, || {
            Arna::init_temp_arenas_with_boxes(4096);
            let scope = lane::scope(Arna::scratch(0));
            let observer = scope.share_with(|| Observer(Default::default()));
            let observed = scope.share_with(|| String::from("dropped first"));
            *observer.0.lock().unwrap() = Some(observed);

            drop(scope);
        });
    }

    /// UB vector #1: barrier cross-pairing exposes uninitialized slots.
    ///
    /// `sync()` performs a bare `barrier.wait()` on the same `Barrier` that
    /// `batch_detached` uses internally, so a lane that only calls `sync()`
    /// releases another lane's `batch` barrier *without lane 0 ever writing
    /// the broadcast slots*. The slots hold `BroadcastPtr::default()` whose
    /// `id` is `TypeId::of::<()>()` and whose `ptr` is null, so a batch with
    /// `P = ()` passes the `downcast` TypeId check and dereferences null.
    #[ignore = "demonstrates UB (null deref): cargo miri test -- --ignored unsound_null_broadcast_slot"]
    #[test]
    pub fn unsound_null_broadcast_slot() {
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 2);
            let scope = lane::scope(Arna::scratch(0));
            if lane::index() == 0 {
                scope.sync();
            } else {
                scope.partition(|| -> &mut [()] { &mut [] });
            }
        });
    }

    /// UB vector #2: use-after-free via thread-local arena reinitialization.
    ///
    /// `Arna::drop` skips the "all checkpoints need to be dropped" assert
    /// when `is_thread_local` is set, and `init_temp_arenas_with_boxes`
    /// sets exactly that flag. Reinitializing while a `Scope` is alive frees
    /// the arena's backing box even though `create_shared` results and the
    /// scope's `Erased` drop chain still point into it.
    #[ignore = "demonstrates UB (use-after-free): cargo miri test -- --ignored unsound_arena_reinit_uaf"]
    #[test]
    pub fn unsound_arena_reinit_uaf() {
        lane::launch(1, || {
            Arna::init_temp_arenas_with_boxes(1024 * 2);
            let scope = lane::scope(Arna::scratch(0));
            let shared = scope.create_shared(String::from("hello"));
            Arna::init_temp_arenas_with_boxes(1024 * 2);
            let _ = shared.len();
            drop(scope);
        });
    }

    /// UB vector #3: divergent scope drop desynchronizes the barrier protocol.
    ///
    /// Barriers do not distinguish call sites: lane 0's `Scope::drop` sync
    /// pairs with lane 1's next `batch` barrier. Lane 1 then reads stale
    /// broadcast slots while lane 0 (already past the drop, in a new scope's
    /// batch) concurrently writes them through the `UnsafeCell` — a data
    /// race with no happens-before edge. Additionally lane 1's stale `broad`
    /// pointer aliases lane 0's arena memory that was reset by the dropped
    /// checkpoint, and lane 0 reads `broad[1]` that lane 1 never initialized.
    #[ignore = "demonstrates UB (data race + uninit read): cargo miri test -- --ignored unsound_divergent_scope_drop_race"]
    #[test]
    pub fn unsound_divergent_scope_drop_race() {
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 2);
            if lane::index() == 0 {
                let scope = lane::scope(Arna::scratch(0));
                scope.broadcast(0u8);
                drop(scope);
                let scope2 = lane::scope(Arna::scratch(0));
                scope2.broadcast(1u8);
            } else {
                let scope = lane::scope(Arna::scratch(0));
                scope.broadcast(2u8);
                scope.broadcast(3u8);
            }
        });
    }

    /// UB vector #4: divergent group projections create multiple "leaders".
    ///
    /// `scope_with_group_projection` trusts each lane's own slice: group
    /// assignment and intra-group rank come from the *local* lane's
    /// projection, while the `GlobalState`s (and their barrier counts) are
    /// built from *lane 0's* projection. With divergent projections, lanes
    /// 1 and 2 land in a group whose barrier has count 1 (i.e. no
    /// synchronization at all) while holding ranks 1 and 2. They then either
    /// read the never-written default `BroadcastPtr` (whose `()` TypeId
    /// passes the `P = ()` downcast and whose null pointer is dereferenced),
    /// race lane 0's slot writes, and/or write past the 1-element broadcast
    /// buffer with their out-of-range ranks.
    #[ignore = "demonstrates UB (null deref / data race / OOB write): cargo miri test -- --ignored unsound_divergent_group_projection"]
    #[test]
    pub fn unsound_divergent_group_projection() {
        lane::launch(3, || {
            Arna::init_temp_arenas_with_boxes(1024 * 2);
            let proj: &[usize] =
                if lane::index() == 0 { &[0, 1, 1] } else { &[0, 0, 0] };
            let scope =
                lane::scope_with_group_projection(Arna::scratch(0), Some(proj));
            scope.broadcast(1u8);
        });
    }

    /// UB vector #5: lifetime confinement breach via type-matched slots.
    ///
    /// `scope_group_with`'s internal `perform` publishes its freshly created
    /// `Vec<Arc<GlobalState>>` through the *parent* broadcast slots with
    /// `S = [Arc<GlobalState>]`, `B = P = ()`. Another lane can match all
    /// three slot types with `share_with` (its closure never runs — only
    /// lane 0 executes the publishing side of a batch) and thereby obtain a
    /// `&[Arc<GlobalState>]` pointing into a `Vec` owned by *lane 0's group
    /// scope*. Lane 0 then drops that scope, freeing the `Vec`'s heap
    /// buffer, while lane 1's reference (tied to *its own* still-alive
    /// scope) remains valid per the type system — a use-after-free.
    #[ignore = "demonstrates UB (use-after-free): cargo miri test -- --ignored unsound_cross_scope_lifetime_escape"]
    #[test]
    pub fn unsound_cross_scope_lifetime_escape() {
        use alloc::sync::Arc;
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 4);
            let scope = lane::scope(Arna::scratch(0));
            if lane::index() == 0 {
                let group =
                    lane::scope_group_with(Arna::scratch(&**scope), |i| i);
                drop(group);
                scope.sync();
            } else {
                let stolen: &[Arc<lane::GlobalState>] = scope
                    .batch(
                        || -> &mut [()] { &mut [] },
                        || -> &[Arc<lane::GlobalState>] { &[] },
                        || [0u8; 256],
                    )
                    .1;
                scope.sync();
                scope.sync();
                scope.sync();
                let _clone = stolen[0].clone();
            }
        });
    }

    /// UB vector #6: broadcast value destructors race the leader's arena reuse.
    ///
    /// Every lane registers a `Dropper` for its own broadcast slot, and each
    /// slot lives in *lane 0's* arena. `Scope::drop` runs the drop barrier
    /// *before* the `to_drop` chain (struct fields drop after `Drop::drop`
    /// returns), so once the barrier releases there is no synchronization
    /// between lane 1's `Dropper` reading/freeing the `String` header at
    /// `broad[1]` and lane 0 resetting its checkpoint and reallocating over
    /// the same memory. This needs no divergence: fully convergent code that
    /// broadcasts a `Drop` type and then reuses the arena is unsound.
    #[ignore = "demonstrates UB (data race / invalid free): MIRIFLAGS=\"-Zmiri-preemption-rate=0.5\" cargo miri test -- --ignored unsound_broadcast_drop_races_arena_reuse"]
    #[test]
    pub fn unsound_broadcast_drop_races_arena_reuse() {
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 4);
            {
                let scope = lane::scope(Arna::scratch(0));
                scope.broadcast(String::from("hello from a lane"));
            }
            if lane::index() == 0 {
                let cp = Arna::scratch(0);
                let buf: &mut [u8] = cp.alloc_default::<u8>(1024).unwrap();
                for _ in 0..1000 {
                    buf.fill(0xAA);
                }
            }
        });
    }

    /// UB vector #7: batch barriers carry no scope identity.
    ///
    /// Every `batch` waits with `Usage::default()`, and the scope ids used by
    /// `Scope::drop` are *broadcast from lane 0*, so two sibling scopes pair
    /// up silently even when the lanes operate on different ones. Here lane 1
    /// calls `u2.broadcast` while lane 0 runs `s1.broadcast`: the barrier
    /// pairs them (identical closure types), and lane 1 receives a slice into
    /// lane 0's *s1* arena while believing it is tied to `u2`'s lifetime.
    /// Lane 0 then drops `s1`, resets the checkpoint, and reuses the memory
    /// while lane 1 is still reading its "u2" slice — a cross-thread data
    /// race from safe code, with not a single usage-assert tripping.
    #[ignore = "demonstrates UB (data race): MIRIFLAGS=\"-Zmiri-preemption-rate=0.5\" cargo miri test -- --ignored unsound_sibling_scope_cross_pairing"]
    #[test]
    pub fn unsound_sibling_scope_cross_pairing() {
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 4);
            if lane::index() == 0 {
                let s1 = lane::scope(Arna::scratch(0));
                let s2 = lane::scope(Arna::scratch(&**s1));
                s1.broadcast(0u8);
                drop(s1);
                let cp = Arna::scratch(&**s2);
                let buf: &mut [u8] = cp.alloc_default::<u8>(1024).unwrap();
                for _ in 0..1000 {
                    buf.fill(0xBB);
                }
                drop(s2);
            } else {
                let u1 = lane::scope(Arna::scratch(0));
                let u2 = lane::scope(Arna::scratch(&**u1));
                let stolen = u2.broadcast(7u8);
                drop(u1);
                for _ in 0..1000 {
                    std::hint::black_box(stolen[0]);
                }
                drop(u2);
            }
        });
    }

    /// UB vector #8: leaking the scope frees the leader's arena under its peers.
    ///
    /// `Arna::drop` skips the "all checkpoints need to be dropped" assert when
    /// `is_thread_local` is set, so a leaked (`mem::forget`ed) scope does not
    /// stop the lane thread from exiting — and thread exit runs the
    /// thread-local arena's destructor, freeing the backing box. Peers still
    /// hold `&S` (from `share`/`broadcast`) pointing into the leader's arena
    /// and read freed memory. Unlike vector #2 this needs no
    /// reinitialization: plain safe `mem::forget` plus thread exit suffices,
    /// because the drop barrier that would have protected the peers is
    /// skipped entirely.
    #[ignore = "demonstrates UB (use-after-free): MIRIFLAGS=\"-Zmiri-preemption-rate=0.5\" cargo miri test -- --ignored unsound_leaked_scope_frees_leader_arena"]
    #[test]
    pub fn unsound_leaked_scope_frees_leader_arena() {
        use {alloc::sync::Arc, std::sync::atomic::AtomicU8};
        let flag = Arc::new(AtomicU8::new(0));
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 4);
            let scope = lane::scope(Arna::scratch(0));
            let shared = scope.share(String::from("leader arena contents"));
            if lane::index() == 0 {
                core::mem::forget(scope);
                flag.store(1, Ordering::Relaxed);
            } else {
                while flag.load(Ordering::Relaxed) == 0 {
                    std::thread::yield_now();
                }
                for _ in 0..10000 {
                    std::hint::black_box(shared.len());
                }
                core::mem::forget(scope);
            }
        });
    }

    #[ignore = "this will deadlock"]
    #[test]
    pub fn melacious() {
        lane::launch(16, || {
            Arna::init_temp_arenas_with_boxes(128 * 2);

            let scope = lane::scope(Arna::scratch(0));

            if lane::index() == 0 {
                let slice =
                    scope.partition(|| scope.alloc_with(16, || 1).unwrap());

                slice.fill(10);
            } else {
                let slice =
                    scope.partition(|| scope.alloc_with(16, || 1).unwrap());

                slice.fill(10);
            }
        });
    }

    #[ignore = "this will deadlock"]
    #[test]
    pub fn unsound_scope_drop_order() {
        lane::launch(16, || {
            Arna::init_temp_arenas_with_boxes(128 * 2);

            let scope = lane::scope(Arna::scratch(0));
            let scope2 = lane::scope(Arna::scratch(&**scope));

            let slice =
                scope2.partition(|| scope2.alloc_with(16, || 1).unwrap());

            if lane::index() == 0 {
                drop(scope2);
                drop(scope);
            } else {
                drop(scope);
                slice.fill(2);
                drop(scope2);
            }
        });
    }

    // These are safe-code regressions that intentionally demonstrate undefined
    // behavior. Run each test separately under Miri using the command on the test.

    /// The batch and drop barriers do not include the scope depth. A batch using
    /// lane 0's inner scope can therefore pair with another lane's outer scope,
    /// and a throwaway inner scope can subsequently satisfy lane 0's drop barrier.
    /// The returned slice remains tied to the peer's outer scope even though its
    /// Strings have been destroyed with lane 0's inner scope.
    ///
    #[test]
    #[ignore = "intentionally demonstrates undefined behavior under Miri"]
    fn cross_depth_scope_uaf() {
        let inner_dropped = AtomicBool::new(false);

        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(4096);
            let outer = lane::scope(Arna::scratch(0));

            if lane::index() == 0 {
                let inner = lane::scope(Arna::scratch(&**outer));
                let values = inner.broadcast(String::from("leader value"));
                std::hint::black_box(values.len());

                drop(inner);
                inner_dropped.store(true, Ordering::Release);
            } else {
                // This pairs with lane 0's `inner.broadcast`, but the returned
                // lifetime is tied to this lane's `outer` scope.
                let values = outer.broadcast(String::from("peer value"));

                // This DROP rendezvous pairs with lane 0 dropping `inner`.
                let dummy = lane::scope(Arna::scratch(&**outer));
                drop(dummy);

                while !inner_dropped.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }

                // Lane 0 has dropped the String backing this reference.
                std::hint::black_box(values[0].as_bytes()[0]);
            }
        });
    }

    /// A by-value argument may itself contain a borrow. Lane 0 publishes that
    /// borrow as state, but another lane receives it with the lifetime inferred
    /// from its own invocation and can use it after lane 0's owner is dropped.
    ///
    #[cfg(false)]
    #[test]
    #[ignore = "intentionally demonstrates undefined behavior under Miri"]
    fn argument_lifetime_uaf() {
        let owner_dropped = AtomicBool::new(false);

        lane::launch(2, || {
            fn return_argument<'a>(
                _: &'a lane::ScopeMemory<'_>,
                value: &'a str,
            ) -> &'a str {
                value
            }

            Arna::init_temp_arenas_with_boxes(4096);
            let scope = lane::scope(Arna::scratch(0));
            let owner = String::from("borrowed state");
            let shared = scope.share_with(owner.as_str(), return_argument);

            if lane::index() == 0 {
                std::hint::black_box(shared.len());
                drop(owner);
                owner_dropped.store(true, Ordering::Release);
                scope.sync();
            } else {
                while !owner_dropped.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }

                // `shared` physically contains lane 0's now-dangling `&str`.
                std::hint::black_box(shared.as_bytes()[0]);
                scope.sync();
            }
        });
    }

    /// Group sizes are accumulated in `u8`. With overflow checks disabled, 256
    /// entries in one group wrap its barrier count to zero. The next broadcast
    /// writes into the resulting zero-length allocation.
    ///
    #[test]
    #[ignore = "intentionally demonstrates release-mode undefined behavior under Miri"]
    fn group_count_overflow_oob() {
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(4096);
            let projection = [0usize; 256];
            let scope = lane::scope_with_group_projection(
                Arna::scratch(0),
                Some(&projection),
            );

            scope.broadcast(1u8);
        });
    }
}
