use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};

/// State shared between [`Observer`] and [`AsyncEvents`]
struct Shared<T> {
    result: Option<T>,
    /// A waker is used to tell the task execute that a futures task may have proceeded and it is
    /// sensible to poll them again. This one offers methods to make sure only Futures for the
    /// events those status may have changed get woken.
    waker: Option<Waker>,
}

/// A Future which is completed, once its associated event is resolved. See
/// [`AsyncEvents::output_of`].
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
                    waker.clone_from(cx.waker())
                } else {
                    shared.waker = Some(cx.waker().clone());
                }
                Poll::Pending
            }
            Some(_) => Poll::Ready(shared.result.take().unwrap()),
        }
    }
}

/// Allows to create futures which will not complete until an associated event id is resolved. This
/// is useful for creating futures waiting for completion on external events which are driven to
/// completion outside of the current process.
///
/// ```
/// use std::time::Duration;
/// use async_events::AsyncEvents;
///
/// async fn foo(events: &AsyncEvents<u32, &'static str>) {
///     // This is a future waiting for an event with key `1` to resover its output.
///     let observer = events.output_of(1);
///     // We can have multiple observers for the same event, if we want to.
///     let another_observer = events.output_of(1);
///
///     // This will block until the event is resolved
///     let result = observer.await;
///     // Do something awesome with result
///     println!("{result}");
/// }
///
/// async fn bar(events: &AsyncEvents<u32, &'static str>) {
///     // All observers waiting for `1` wake up and their threads may continue. You could resolve
///     // multiple events at once with the same result. This wakes up every observer associated
///     // with the event.
///     events.resolve_all_with(&[1], "Hello, World");
/// }
/// ```
pub struct AsyncEvents<K, T> {
    wakers: Mutex<Vec<Promise<K, T>>>,
}

impl<K, T> AsyncEvents<K, T> {
    pub fn new() -> Self {
        Self {
            wakers: Mutex::new(Vec::new()),
        }
    }
}

impl<K, T> Default for AsyncEvents<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> AsyncEvents<K, V>
where
    K: Eq,
{
    /// A future associated with a peer, which can be resolved using `resolve_with`. You can call
    /// this method repeatedly to create multiple observers waiting for the same event.
    #[deprecated(note = "Please use output_of instead")]
    pub fn wait_for_output(&self, event_id: K) -> Observer<V> {
        let strong = Arc::new(Mutex::new(Shared {
            result: None,
            waker: None,
        }));
        let weak = Arc::downgrade(&strong);
        {
            let mut wakers = self.wakers.lock().unwrap();
            wakers.retain(|promise| !promise.is_orphan());
            wakers.push(Promise {
                key: event_id,
                shared: weak,
            });
        }
        Observer { shared: strong }
    }

    /// A future associated with a peer, which can be resolved using `resolve_with`. You can call
    /// this method repeatedly to create multiple observers waiting for the same event.
    ///
    /// Events are created **implicitly** by creating futures waiting for them. They are removed
    /// then it is resolved. Waiting on an already resolved event will hang forever.
    ///
    /// ```
    /// use async_events::AsyncEvents;
    ///
    /// # async fn example() {
    /// let events = AsyncEvents::<u32, u32>::new();
    ///
    /// // Event occurs before we created the observer
    /// events.resolve_all_with(&[1], 42);
    ///
    /// // Oh no, event `1` has already been resolved. This is likely to wait forever.
    /// let answer = events.output_of(1).await;
    /// # }
    /// ```
    pub fn output_of(&self, event_id: K) -> Observer<V> {
        let strong = Arc::new(Mutex::new(Shared {
            result: None,
            waker: None,
        }));
        let weak = Arc::downgrade(&strong);
        {
            let mut wakers = self.wakers.lock().unwrap();
            wakers.retain(|promise| !promise.is_orphan());
            wakers.push(Promise {
                key: event_id,
                shared: weak,
            });
        }
        Observer { shared: strong }
    }

    /// Resolves all the pending [`Observer`]s associated with the given ids.
    ///
    /// * `event_ids`: Observers associated with these ids are resolved. It would be typical to call
    ///   this method with only one element in `event_ids`. However in some error code paths it is
    ///   not unusual that you can rule provide a result (usually `Err`) for many events at once.
    /// * `output`: The result these [`Observer`]s will return in their `.await` call
    pub fn resolve_all_with(&self, event_ids: &[K], output: V)
    where
        V: Clone,
    {
        let mut wakers = self.wakers.lock().unwrap();
        for promise in wakers.iter_mut() {
            if promise.is_match(event_ids) {
                promise.resolve(output.clone())
            }
        }
    }

    /// Resolves all the pending [`Observer`]s. This resolves all events independent of their id.
    /// This might come in useful e.g. during application shutdown.
    /// 
    /// * `output`: The result these [`Observer`]s will return in their `.await` call
    /// * `f`: Function acting as a filter for event ids which are to be resolved, and as a factory
    ///   for their results. If `f` returns `None` observers associated with the event id are not 
    ///   resolved. If `Some` all observers with this Id are resolved.
    pub fn resolve_all_if(&self, f: impl Fn(&K) -> Option<V>) where V: Clone,
    {
        let mut wakers = self.wakers.lock().unwrap();
        for promise in wakers.iter_mut() {
            if let Some(output) = f(&promise.key) {
                promise.resolve(output)
            }
        }
    }

    /// Resolves one pending [`Observer`] associated with the given event id. If no observer with
    /// such an id exists, nothing happens.
    ///
    /// * `event_id`: One [`Observer`] associated with this ids is resolved.
    /// * `output`: The result the [`Observer`] will return in its `.await` call
    pub fn resolve_one(&self, event_id: K, output: V) {
        let mut wakers = self.wakers.lock().unwrap();
        if let Some(promise) = wakers.iter_mut().find(|p| p.key == event_id) {
            promise.resolve(output);
        }
    }
}

/// For every [`Observer`] future, we create an associated promise, which we can use to send the
/// result and notify the async runtime that it should poll the future again.
struct Promise<K, T> {
    /// Identifiere of the event the associated future is waiting on.
    key: K,
    /// Weak reference to the shared result state.
    shared: Weak<Mutex<Shared<T>>>,
}

impl<K, T> Promise<K, T> {
    /// Set result and notify the runtime to poll the observing Future
    fn resolve(&mut self, result: T) {
        if let Some(strong) = self.shared.upgrade() {
            let mut shared = strong.lock().unwrap();
            shared.result = Some(result);
            if let Some(waker) = shared.waker.take() {
                waker.wake()
            }
        }
    }

    /// `true` if the promise key is contained in the query.
    fn is_match(&self, query: &[K]) -> bool
    where
        K: Eq,
    {
        query.contains(&self.key)
    }

    /// No Observer is watining anymore for this promise to be resolved.
    fn is_orphan(&self) -> bool {
        self.shared.strong_count() == 0
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
        let future = pm.output_of(1);
        // Promise not yet fulfilled => Elapses due to timeout.
        timeout(ZERO, future).await.unwrap_err();
    }

    #[tokio::test]
    async fn resolved() {
        let pm = AsyncEvents::new();
        let future = pm.output_of(1);
        pm.resolve_all_with(&[1], 42);
        // Promise fulfilled => Return result
        assert_eq!(42, timeout(ZERO, future).await.unwrap());
    }

    #[tokio::test]
    async fn multiple_observers_resolve_all() {
        let pm = AsyncEvents::new();
        let obs_1 = pm.output_of(1);
        let obs_2 = pm.output_of(1);
        pm.resolve_all_with(&[1], 42);
        assert_eq!(42, timeout(ZERO, obs_1).await.unwrap());
        assert_eq!(42, timeout(ZERO, obs_2).await.unwrap());
    }

    #[tokio::test]
    async fn multiple_observers_resolve_one() {
        let pm = AsyncEvents::new();
        let obs_1 = pm.output_of(1);
        let obs_2 = pm.output_of(1);
        pm.resolve_one(1, 42);
        assert_eq!(42, timeout(ZERO, obs_1).await.unwrap());
        // Second observer times out
        assert!(timeout(ZERO, obs_2).await.is_err());
    }

    /// This has been proven usefull if shutting down an application, and wanting to stop waiting
    /// on all pending futures.
    #[tokio::test]
    async fn resolve_all_observers_with_the_same_output() {
        let pm = AsyncEvents::new();
        let obs_1 = pm.output_of(1);
        let obs_2 = pm.output_of(2);

        // We ignore event ID, we want to give the same result to all observers.
        pm.resolve_all_if(|_event_id| Some(42));
        
        assert_eq!(42, timeout(ZERO, obs_1).await.unwrap());
        assert_eq!(42, timeout(ZERO, obs_2).await.unwrap());
    }
}
