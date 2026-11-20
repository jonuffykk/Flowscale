use std::cell::RefCell;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateWaitableTimerExW, SetWaitableTimerEx, WaitForSingleObject};

pub fn qpc() -> i64 { let mut v = 0; unsafe { let _ = QueryPerformanceCounter(&mut v); } v }
pub fn qpf() -> i64 { let mut v = 0; unsafe { let _ = QueryPerformanceFrequency(&mut v); } v.max(1) }

struct ThreadTimer(HANDLE);
impl Drop for ThreadTimer { fn drop(&mut self) { if !self.0.is_invalid() { unsafe { let _ = CloseHandle(self.0); } } } }
thread_local! {
    static TIMER: RefCell<Option<ThreadTimer>> = const { RefCell::new(None) };
}
fn thread_timer() -> HANDLE {
    TIMER.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            let h = unsafe { CreateWaitableTimerExW(None, None, 0x2, 0x1F0003) }
                .or_else(|_| unsafe { CreateWaitableTimerExW(None, None, 0, 0x1F0003) })
                .unwrap_or_default();
            *t = Some(ThreadTimer(h));
        }
        t.as_ref().unwrap().0
    })
}

#[derive(Default, Clone, Copy, Debug)]
pub struct WaitStats { pub slept_ms: f64, pub spun_ms: f64, pub wake_err_ms: f64, pub late: bool }

pub struct Pacer {
    pub freq: i64,
    milli_fps: u32,
    period: i64, rem: i64, acc: i64,
    pub deadline: i64,
    pub spin_ms: f64,
}

impl Pacer {
    pub fn new() -> Pacer { Pacer { freq: qpf(), milli_fps: 0, period: 0, rem: 0, acc: 0, deadline: 0, spin_ms: 1.0 } }

    pub fn ms(&self, ticks: i64) -> f64 { ticks as f64 * 1000.0 / self.freq as f64 }
    pub fn ticks(&self, ms: f64) -> i64 { (ms * self.freq as f64 / 1000.0) as i64 }

    pub fn set_rate(&mut self, milli_fps: u32) {
        if milli_fps == self.milli_fps { return; }
        self.milli_fps = milli_fps; self.acc = 0; self.deadline = 0;
        if milli_fps > 0 {
            let num = self.freq as i128 * 1_000_000;
            let den = milli_fps as i128 * 1000;
            self.period = (num / den) as i64; self.rem = (num % den) as i64;
        }
    }
    fn den(&self) -> i64 { self.milli_fps as i64 * 1000 }

    pub fn next_deadline(&mut self) -> i64 {
        let now = qpc();
        if self.milli_fps == 0 { return now; }
        if self.deadline == 0 || now - self.deadline > self.period { self.deadline = now; self.acc = 0; }
        self.deadline += self.period; self.acc += self.rem;
        if self.acc >= self.den() { self.acc -= self.den(); self.deadline += 1; }
        self.deadline
    }

    pub fn limit(&mut self) -> WaitStats {
        if self.milli_fps == 0 { return WaitStats::default(); }
        let d = self.next_deadline();
        self.wait_until(d)
    }

    pub fn wait_until(&mut self, due: i64) -> WaitStats {
        let mut st = WaitStats::default();
        let start = qpc();
        if start >= due { st.late = true; return st; }
        let wake_at = due - self.ticks(self.spin_ms);
        if wake_at > start {
            let timer = thread_timer();
            let rel = ((wake_at - start) as i128 * 10_000_000 / self.freq as i128).max(1) as i64;
            let ok = !timer.is_invalid() && unsafe { SetWaitableTimerEx(timer, &-rel, 0, None, None, None, 0).is_ok() };
            if ok { unsafe { WaitForSingleObject(timer, (self.ms(wake_at - start) as u32) + 10); } }
            else { let ms = self.ms(wake_at - start); if ms >= 2.0 { std::thread::sleep(std::time::Duration::from_micros(((ms - 1.0) * 1000.0) as u64)); } }
            let woke = qpc();
            st.slept_ms = self.ms(woke - start);
            let err = self.ms(woke - wake_at);
            st.wake_err_ms = err;
            let target = (err + 0.15).clamp(0.25, 3.0);
            let k = if target > self.spin_ms { 0.5 } else { 0.005 };
            self.spin_ms = (self.spin_ms + (target - self.spin_ms) * k).clamp(0.25, 3.0);
        }
        let s0 = qpc();
        while qpc() < due { std::hint::spin_loop(); }
        st.spun_ms = self.ms(qpc() - s0);
        st
    }
}

pub struct FrameStats { pub times: Vec<f32>, head: usize, pub count: usize }
impl FrameStats {
    pub fn new(cap: usize) -> FrameStats { FrameStats { times: vec![0.0; cap], head: 0, count: 0 } }
    pub fn push(&mut self, ms: f32) { let n = self.times.len(); self.times[self.head] = ms; self.head = (self.head + 1) % n; self.count = (self.count + 1).min(n); }
    pub fn recent(&self, n: usize) -> Vec<f32> {
        let n = n.min(self.count); let cap = self.times.len();
        (0..n).map(|i| self.times[(self.head + cap - n + i) % cap]).collect()
    }
    pub fn hitches(&self) -> (u32, u32) {
        if self.count < 8 { return (0, 0); }
        let v = self.recent(self.count);
        let mut s = v.clone(); s.sort_by(f32::total_cmp);
        let med = s[s.len() / 2];
        (v.iter().filter(|x| **x > med * 1.5).count() as u32, v.iter().filter(|x| **x > med * 2.0).count() as u32)
    }
    pub fn summary(&self) -> (f32, f32, f32, f32) {
        if self.count < 2 { return (0.0, 0.0, 0.0, 0.0); }
        let v = self.recent(self.count);
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
        let mut s = v.clone(); s.sort_by(|a, b| b.total_cmp(a));
        let tail = |p: f32| { let k = ((s.len() as f32 * p).ceil() as usize).max(1); let m = s[..k].iter().sum::<f32>() / k as f32; if m > 0.0 { 1000.0 / m } else { 0.0 } };
        (if mean > 0.0 { 1000.0 / mean } else { 0.0 }, tail(0.01), tail(0.001), sd)
    }
}
