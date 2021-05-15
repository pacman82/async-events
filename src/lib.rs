use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};

/// State shared between Future and Wakers
struct Shared<T> {
    result: Option<T>,
    /// A waker is used to tell the task execute that a futures task may have proceeded and it is
    /// sensible to poll them again. This one offers methods to make sure only Futures for the
    /// events those status may have changed get woken.
    waker: Option<Waker>,
}

/// A Future which is completed, once its associated event is resolved. See
/// [`AsyncEvents::wait_for_output`].
pub struct Observer<T> {
    shared: Arc<Mutex<Shared<T>>>,
}

impl<T> Future for Observer<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut shared = self.shared.lock().unwrap();
        match &shared.result {
            None => {
                if let Some(ref mut waker) = &mut shared.waker {
                    // If a waker has been previously set, let's reuse the resources from the old
                    // one, rather than allocating a new one.
                    waker.clone_from(&cx.waker())
                } else {
                    shared.waker = Some(cx.waker().clone());
                }
                Poll::Pending
            }
            Some(_) => Poll::Ready(shared.result.take().unwrap()),
        }
    }
}

/// Allows to create futures which won't complete until an associated event id is resolved. This
/// is useful for creating futures waiting for completion on external events which are driven to
/// completion outside of the current process. Waiting 
pub struct AsyncEvents<K, T> {
    wakers: Mutex<Vec<(K, Weak<Mutex<Shared<T>>>)>>,
}

impl<K, T> AsyncEvents<K, T> {
    pub fn new() -> Self {
        Self {
            wakers: Mutex::new(Vec::new()),
        }
    }
}

impl<K, V> AsyncEvents<K, V> where K: Eq {
    /// A future associated with a peer, which can be resolved using `resolve_with`.
    ///
    /// Attention: Do not call this method while holding a lock to `leases`.
    pub fn wait_for_output(&self, event_id: K) -> Observer<V> {
        let strong = Arc::new(Mutex::new(Shared {
            result: None,
            waker: None,
        }));
        let weak = Arc::downgrade(&strong);
        {
            let mut wakers = self.wakers.lock().unwrap();
            wakers.retain(|(_event_id, r)| r.strong_count() != 0);
            wakers.push((event_id, weak));
        }
        Observer { shared: strong }
    }

    /// Resolves all the pending futures associated with the given ids.
    ///
    /// * `event_ids`: Observers associated with these ids are resolved
    /// * `output`: The result these futures will return in their `.await` call
    pub fn resolve_all_with(&self, event_ids: &[K], output: V) where V: Clone {
        let mut wakers = self.wakers.lock().unwrap();
        for (event_id, weak) in wakers.iter_mut() {
            if event_ids.contains(event_id) {
                if let Some(strong) = weak.upgrade() {
                    let mut shared = strong.lock().unwrap();
                    shared.result = Some(output.clone());
                    if let Some(waker) = shared.waker.take() {
                        waker.wake()
                    }
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {

    use std::time::Duration;

    use super::AsyncEvents;
    use tokio::{self, time::timeout};

    const ZERO: Duration = Duration::from_secs(0);

    #[tokio::test]
    async fn pending() {

        let pm: AsyncEvents<i32, ()> = AsyncEvents::new();
        let future = pm.wait_for_output(1);
        // Promise not yet fulfilled => Elapses due to timeout.
        timeout(ZERO, future).await.unwrap_err();
    }

    #[tokio::test]
    async fn resolved() {
        let pm = AsyncEvents::new();
        let future = pm.wait_for_output(1);
        pm.resolve_all_with(&[1], 42);
        // Promise fulfilled => Return result
        assert_eq!(42, timeout(ZERO, future).await.unwrap());
    }

    #[tokio::test]
    async fn multiple_observers() {
        let pm = AsyncEvents::new();
        let obs_1 = pm.wait_for_output(1);
        let obs_2 = pm.wait_for_output(1);
        pm.resolve_all_with(&[1], 42);
        assert_eq!(42, timeout(ZERO, obs_1).await.unwrap());
        assert_eq!(42, timeout(ZERO, obs_2).await.unwrap());
    }
}
