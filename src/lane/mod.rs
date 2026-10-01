use {
    crate::{Arna, Checkpoint},
    alloc::sync::Arc,
    core::{
        any::{Any, TypeId},
        cell::{Cell, RefCell, UnsafeCell},
        ops::{Deref, Range},
        ptr::NonNull,
    },
    std::sync::Barrier,
};

pub struct BroadcastPtr {
    ptr: *const (),
    id: TypeId,
}

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
                        *s.borrow_mut() =
                            Some(LocalState { global, index: lane_index });
                    });

                    main();
                })
                .expect("wut");
        }
    })
}

fn with_local<T>(with: impl FnOnce(&mut LocalState) -> T) -> T {
    LOCAL_STATE.with(|s| {
        with(
            s.borrow_mut()
                .as_mut()
                .expect("not called withing the lane thread"),
        )
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

pub struct Scope<'b> {
    to_drop: Cell<Erased>,
    checkpoint: Checkpoint<'b>,
    // Allocated on checkpoint
    sync_barier: NonNull<Barrier>,
    prev_local: Option<LocalState>,
    pub group: u8,
}

impl<'b> Deref for Scope<'b> {
    type Target = Checkpoint<'b>;

    fn deref(&self) -> &Self::Target {
        &self.checkpoint
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
    with_local(|l| l.index)
}

pub fn count() -> usize {
    with_local(|l| l.global.count)
}

pub fn scope(checkpoint: Checkpoint) -> Scope {
    scope_with_group_projection(checkpoint, None)
}

pub fn scope_group_with(
    checkpoint: Checkpoint,
    mut partitioner: impl FnMut(u8) -> u8,
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
    group_projection: Option<&[u8]>,
) -> Scope<'a> {
    // NOTE: outside code can not reproduce the call here, they dont have access to the
    // type id of this type so they can't call the broadcast to smuggle out the state we
    // have here
    struct TamperMarker;

    #[derive(Clone)]
    struct Smuggle(NonNull<Barrier>);

    unsafe impl Send for Smuggle {}
    unsafe impl Sync for Smuggle {}

    unsafe fn perform<'a, 'b>(
        checkpoint: &'a Checkpoint,
        to_drop: &Cell<Erased>,
        groups: &[u8],
    ) -> (u8, LocalState, NonNull<Barrier>) {
        let index = index();

        let current = groups[index];
        let index = groups[..index].iter().filter(|&&v| v == current).count();

        let mut pattern = [0u8; 256];
        pattern[..groups.len()].copy_from_slice(groups);

        let (broad, shared, _) = unsafe {
            batch_detached(
                checkpoint,
                to_drop,
                pattern,
                || {
                    let group_count = groups
                        .iter()
                        .copied()
                        .max()
                        .expect("we absolutely have at least one lane")
                        + 1;

                    let thread_counts = checkpoint
                        .alloc_default::<u8>(group_count as usize)
                        .unwrap();
                    for &group in groups {
                        thread_counts[group as usize] += 1;
                    }

                    let slots = thread_counts
                        .iter()
                        .map(|&count| {
                            (
                                Arc::new(GlobalState::new(count as usize)),
                                Smuggle(NonNull::from_ref(
                                    checkpoint
                                        .create(Barrier::new(count as usize)),
                                )),
                            )
                        })
                        .collect::<Vec<_>>();

                    create_shared(checkpoint, to_drop, slots).as_slice()
                },
                TamperMarker,
                |_| -> &mut [TamperMarker] { &mut [] },
            )
        };

        let (global, Smuggle(barrier)) = shared[current as usize].clone();

        if !broad.iter().all(|&v| v == broad[index]) {
            // NOTE: this is required and also sound since we just successfully broadcasted
            // with a private type
            with_local(|l| l.global.share_barier.wait());
            panic!("projection mismatch");
        }

        (
            current,
            with_local(|l| core::mem::replace(l, LocalState { global, index })),
            barrier,
        )
    }

    let mut prev_local = None;
    let mut group = 0;
    let bariera;
    let to_drop = Default::default();
    if let Some(group_projection) = group_projection {
        let (new_group, new_prev_local, barier) =
            unsafe { perform(&checkpoint, &to_drop, group_projection) };
        group = new_group;
        prev_local = Some(new_prev_local);
        bariera = barier;
    } else {
        let count = count();
        bariera = unsafe {
            NonNull::from_ref(
                batch_detached(
                    &checkpoint,
                    &to_drop,
                    (),
                    || checkpoint.create(Barrier::new(count)),
                    TamperMarker,
                    |_| -> &mut [TamperMarker] { &mut [] },
                )
                .1,
            )
        }
    }

    Scope { checkpoint, to_drop, prev_local, group, sync_barier: bariera }
}

pub unsafe fn create_shared<'a, S>(
    checkpoint: &'a Checkpoint,
    head: &Cell<Erased>,
    s: S,
) -> &'a mut S {
    if core::mem::needs_drop::<S>() {
        let state = checkpoint.create(EraseNode { s, _next: head.take() });
        let ptr = state as *mut _ as *mut _;
        head.set(Erased {
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
        checkpoint.create(s)
    }
}

/// # Safety
///
/// Caller must guarantee the `checkpoint` is dropped after `to_drop`
pub unsafe fn batch_detached<
    'a,
    B: Sync + Send + Any,
    S: Sync + Send + Any + ?Sized,
    SP,
    P: Send + Sync + Any,
>(
    checkpoint: &'a Checkpoint,
    to_drop: &Cell<Erased>,
    broadcast: B,
    state: impl FnOnce() -> &'a S,
    partition: SP,
    compute: impl FnOnce(&mut SP) -> &mut [P],
) -> (&'a [B], &'a S, &'a mut [P]) {
    with_local(|local| {
        let part;
        let state_ptr;
        let broad;
        if local.index == 0 {
            let partition_state =
                unsafe { create_shared(checkpoint, to_drop, partition) };

            part = compute(partition_state);
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

            broad = checkpoint.alloc_uninit::<B>(local.global.count);
            unsafe {
                *local.global.broadcast_ptrs[2].get() = BroadcastPtr {
                    ptr: &broad as *const _ as *const _,
                    id: TypeId::of::<B>(),
                };
            }
        }

        println!("in {:?}", local.index);

        local.global.share_barier.wait();
        println!("out {:?}", local.index);

        let part = unsafe {
            (*local.global.broadcast_ptrs[0].get())
                .downcast::<P, *mut [P]>()
                .ok_or("partition")?
        };

        let state = unsafe {
            (*local.global.broadcast_ptrs[1].get())
                .downcast::<S, &S>()
                .ok_or("state")?
        };

        let broad = unsafe {
            (*local.global.broadcast_ptrs[2].get())
                .downcast::<B, *mut [B]>()
                .ok_or("broadcast")?
        };
        unsafe { (broad as *mut B).add(local.index).write(broadcast) };

        local.global.share_barier.wait();

        let range =
            task_slice_bounds(part.len(), local.global.count, local.index);

        Ok::<_, &'static str>((unsafe { &*broad }, state, unsafe {
            core::slice::from_raw_parts_mut(
                (part as *mut P).add(range.start),
                range.len(),
            )
        }))
    })
    .expect("type mismatch")
}

impl<'b> Scope<'b> {
    pub fn create_shared<S: 'static>(&self, s: S) -> &mut S {
        unsafe { create_shared(&self.checkpoint, &self.to_drop, s) }
    }

    pub fn batch<
        'a,
        B: Sync + Send + Any,
        S: Sync + Send + Any + ?Sized,
        SP,
        P: Send + Sync + Any,
    >(
        &'a self,
        broadcast: B,
        state: impl FnOnce() -> &'a S,
        partition: SP,
        compute: impl FnOnce(&mut SP) -> &mut [P],
    ) -> (&'a [B], &'a S, &'a mut [P]) {
        unsafe {
            batch_detached(
                &self.checkpoint,
                &self.to_drop,
                broadcast,
                state,
                partition,
                compute,
            )
        }
    }

    pub fn broadcast<'a, I: Send + Sync + Any>(&self, input: I) -> &[I] {
        self.batch(input, || &(), (), |_| -> &mut [()] { &mut [] }).0
    }

    pub fn share<'a, S: Send + Sync + Any>(&'a self, state: S) -> &'a S {
        self.share_with(|| self.create_shared(state))
    }

    pub fn share_with<'a, S: Send + Sync + Any + ?Sized>(
        &'a self,
        state: impl FnOnce() -> &'a S,
    ) -> &'a S {
        self.batch((), state, (), |_| -> &mut [()] { &mut [] }).1
    }

    pub fn partition<'a, S, T: Send + Sync + Any>(
        &'a self,
        state: S,
        compute: impl FnOnce(&mut S) -> &mut [T],
    ) -> &'a mut [T] {
        self.batch((), || &(), state, compute).2
    }

    pub fn sync(&self) {
        unsafe { self.sync_barier.as_ref().wait() };
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if let Some(prev_local) = &mut self.prev_local {
            with_local(|l| core::mem::swap(l, prev_local));
        }

        self.sync();
    }
}

#[cfg(test)]
mod test {
    use {
        crate::{Arna, lane},
        core::{
            iter,
            sync::atomic::{AtomicUsize, Ordering},
        },
    };

    #[test]
    pub fn sanity() {
        lane::launch(2, || {
            Arna::init_temp_arenas_with_boxes(1024 * 2);

            {
                let scope = lane::scope(Arna::scratch(0));

                let slice = scope.partition(vec![], |vck| {
                    vck.extend(iter::repeat_n(1, 1024));
                    &mut vck[..]
                });

                let sum = slice.iter().sum::<usize>();

                let accum2 = scope.share(AtomicUsize::new(0));

                accum2.fetch_add(sum, Ordering::Relaxed);

                let accum1 = scope.broadcast(sum);

                assert_eq!(accum1.iter().sum::<usize>(), 1024);
                assert_eq!(accum2.load(Ordering::Relaxed), 1024);

                {
                    enum GroupKind {
                        Eaven(usize),
                        Odd(usize),
                    }

                    let scratch = Arna::scratch(&*scope);
                    let split_scope =
                        lane::scope_group_with(scratch, |i| i % 2);

                    let slice = scope.partition(vec![], |vck| {
                        vck.extend(iter::repeat_n(1, 1024));
                        &mut vck[..]
                    });

                    let sum = slice.iter().sum::<usize>();

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
                scope.partition((), |_| -> &mut [()] { &mut [] });
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
            let proj: &[u8] =
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
                    lane::scope_group_with(Arna::scratch(&*scope), |i| i);
                drop(group);
                scope.sync();
            } else {
                let stolen: &[Arc<lane::GlobalState>] = scope
                    .batch(
                        [0u8; 256],
                        || -> &[Arc<lane::GlobalState>] { &[] },
                        (), // no longer possible
                        |_| -> &mut [()] { &mut [] },
                    )
                    .1;
                scope.sync();
                scope.sync();
                scope.sync();
                let _clone = stolen[0].clone();
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
                let slice = scope.partition(vec![], |vck| {
                    vck.extend(iter::repeat_n(1u8, 1024));
                    &mut vck[..]
                });

                slice.fill(10);
            } else {
                let slice = scope.partition(vec![], |vck| {
                    vck.extend(iter::repeat_n(1usize, 1024));
                    &mut vck[..]
                });

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
            let scope2 = lane::scope(Arna::scratch(&*scope));

            let slice = scope2.partition(vec![], |vck| {
                vck.extend(iter::repeat_n(1usize, 1024));
                &mut vck[..]
            });

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
}
