//! Runs independent Jev requests side by side.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Requests in flight at once.
const WORKERS: usize = 8;

/// `f` applied to every item, with results in the items' order.
pub fn map<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let results: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..WORKERS.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    *results[i].lock().unwrap() = Some(f(item));
                }
            });
        }
    });
    results
        .into_iter()
        .map(|r| r.into_inner().unwrap().expect("every item is mapped"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_order() {
        let items: Vec<u32> = (0..50).collect();
        assert_eq!(
            super::map(&items, |n| n * 2),
            (0..50).map(|n| n * 2).collect::<Vec<_>>()
        );
    }
}
