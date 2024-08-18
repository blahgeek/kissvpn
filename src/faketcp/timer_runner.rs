use std::sync::{Arc, Condvar, Mutex};
use std::collections::BTreeMap;


#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub struct TimerJobId(std::time::Instant, i64);

pub type TimerJob = Box<dyn FnOnce() -> () + 'static + Send>;

type JobQueue = BTreeMap<TimerJobId, TimerJob>;

/// Return duration to first job in queue, or None if no job in queue.
/// When first job is already due, return zero duration
fn get_first_job_delay(q: &JobQueue) -> Option<std::time::Duration> {
    let now = std::time::Instant::now();
    q.first_key_value()
        .map(|(k,_)| if now > k.0 { now - k.0 } else { std::time::Duration::ZERO })
}

struct TimerRunnerState {
    jobs: Mutex<JobQueue>,
    job_available_cond: Condvar,
    keep_running: std::sync::atomic::AtomicBool,
}

impl TimerRunnerState {
    fn run_thread_loop(&self) -> () {
        const MAX_WAIT_TIME: std::time::Duration = std::time::Duration::from_secs(1);
        while self.keep_running.load(std::sync::atomic::Ordering::SeqCst) {
            let guard = self.jobs.lock().unwrap();
            let wait_timeout = get_first_job_delay(&guard).unwrap_or(MAX_WAIT_TIME);
            let (mut guard, _) = self.job_available_cond.wait_timeout_while(guard, wait_timeout, |jobs| {
               get_first_job_delay(jobs) != Some(std::time::Duration::ZERO)
            }).unwrap();
            if get_first_job_delay(&guard) == Some(std::time::Duration::ZERO) {
                let job = guard.pop_first().unwrap().1;
                drop(guard);
                job();
            }
        }
    }
}

pub struct TimerRunner {
    next_job_id: std::sync::atomic::AtomicI64,
    state: Arc<TimerRunnerState>,
    thread: Option<std::thread::JoinHandle<()>>,  // for join() in drop()
}

impl Drop for TimerRunner {
    fn drop(&mut self) {
        self.state.keep_running.store(false, std::sync::atomic::Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

impl TimerRunner {
    pub fn new() -> Self {
        let state = Arc::new(TimerRunnerState {
            jobs: Mutex::new(JobQueue::new()),
            job_available_cond: Condvar::new(),
            keep_running: std::sync::atomic::AtomicBool::new(true),
        });
        let thread = {
            let state = state.clone();
            std::thread::spawn(move || {
                state.run_thread_loop();
            })
        };
        Self {
            next_job_id: std::sync::atomic::AtomicI64::new(0),
            state,
            thread: Some(thread),
        }
    }

    pub fn schedule<F>(&self, delay: std::time::Duration, job: F) -> TimerJobId
    where F: FnOnce() -> () + 'static + Send {
        let job_id = TimerJobId(
            std::time::Instant::now() + delay,
            self.next_job_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        self.state.jobs.lock().unwrap().insert(job_id.clone(), Box::new(job));
        self.state.job_available_cond.notify_one();
        return job_id;
    }

    pub fn cancel(&self, job_id: &TimerJobId) -> bool {
        self.state.jobs.lock().unwrap().remove(job_id).is_some()
    }
}


#[cfg(test)]
mod tests {
    use std::ops::Deref;
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_timers() {
        let result = Arc::new(Mutex::new(String::new()));
        let make_task =
            |result: Arc<Mutex<String>>, s: String| {
                move || {
                    result.lock().unwrap().push_str(&s)
                }
            };

        let runner = TimerRunner::new();
        let joba = runner.schedule(Duration::from_millis(100), make_task(result.clone(), "a".into()));
        runner.schedule(Duration::from_millis(200), make_task(result.clone(), "b".into()));
        runner.schedule(Duration::from_millis(100), make_task(result.clone(), "c".into()));

        assert!(runner.cancel(&joba));

        std::thread::sleep(Duration::from_micros(300));
        assert_eq!(result.lock().unwrap().deref(), "cb");
    }
}
