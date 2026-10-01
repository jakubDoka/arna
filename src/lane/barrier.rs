use std::sync::{Condvar, Mutex};

pub struct Barrier {
    lock: Mutex<BarrierState>,
    cvar: Condvar,
    num_threads: usize,
}

struct BarrierState {
    count: usize,
    generation_id: usize,
    usage: Usage,
}

#[derive(Default, PartialEq, Eq, Debug)]
pub struct Usage {
    pub object_id: [u64; 2],
    pub purpose_id: u64,
}

impl Barrier {
    pub fn new(n: usize) -> Barrier {
        Barrier {
            lock: Mutex::new(BarrierState {
                count: 0,
                generation_id: 0,
                usage: Usage::default(),
            }),
            cvar: Condvar::new(),
            num_threads: n,
        }
    }

    pub fn wait(&self, usage: Usage) -> bool {
        let mut lock = self.lock.lock().expect("we dont do that");

        if lock.count == 0 {
            lock.usage = usage;
        } else {
            assert_eq!(
                lock.usage, usage,
                "usage does not match for this cycle"
            );
        }

        let local_gen = lock.generation_id;
        lock.count += 1;
        if lock.count < self.num_threads {
            drop(
                self.cvar
                    .wait_while(lock, |state| local_gen == state.generation_id)
                    .expect("no poison to see"),
            );
            false
        } else {
            lock.count = 0;
            lock.generation_id = lock.generation_id.wrapping_add(1);
            self.cvar.notify_all();
            true
        }
    }
}
