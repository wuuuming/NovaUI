use novaui::image::cache::WeakCache;
use std::sync::{mpsc, Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

#[test]
fn coalesces_loading_without_pinning_completed_payloads() {
    let cache = Arc::new(WeakCache::<u32, Arc<Vec<u8>>>::new(8));
    let start = Arc::new(Barrier::new(9));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (attempts, attempt_rx) = mpsc::channel();
    let (started, started_rx) = mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..8 {
        let (cache, start, gate, attempts, started) =
            (cache.clone(), start.clone(), gate.clone(), attempts.clone(), started.clone());
        threads.push(std::thread::spawn(move || {
            start.wait();
            attempts.send(()).unwrap();
            cache
                .get_or_load(1, || {
                    started.send(()).unwrap();
                    let (lock, wake) = &*gate;
                    let mut ready = lock.lock().unwrap();
                    while !*ready {
                        ready = wake.wait(ready).unwrap();
                    }
                    Some(Arc::new(vec![1, 2, 3, 255]))
                })
                .unwrap()
        }));
    }
    start.wait();
    for _ in 0..8 {
        attempt_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let values: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(values.iter().all(|v| Arc::ptr_eq(v, &values[0])));
    assert_eq!(cache.stats().loads, 1);
    assert_eq!(cache.stats().hits + cache.stats().waits, 7);
    let weak = Arc::downgrade(&values[0]);
    drop(values);
    assert!(weak.upgrade().is_none(), "completed cache must not keep decoded pixels alive");
    let _again = cache.get_or_load(1, || Some(Arc::new(vec![1, 2, 3, 255]))).unwrap();
    assert_eq!(cache.stats().loads, 2);
    assert!(cache.get_or_load(2, || None).is_none());
    assert!(cache.get_or_load(2, || Some(Arc::new(vec![4]))).is_some(), "failures must be retryable");
    let panic = std::panic::catch_unwind(|| cache.get_or_load(3, || panic!("test decoder failure")));
    assert!(panic.is_err());
    assert!(cache.get_or_load(3, || Some(Arc::new(vec![5]))).is_some(), "panic must not leave a stuck flight");
}
