//! Songs from outside the library whose file has just landed in the music folder: the
//! download service tells this, and whoever listens hears the song's key (`provider:id`, as
//! the acquisition tracker keys it) and where its file is.

use std::sync::Arc;

use parking_lot::Mutex;

/// Told of each landed file: the song's key and the file's path.
pub type ArrivalListener = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Default)]
pub struct DownloadArrivals {
    listeners: Mutex<Vec<ArrivalListener>>,
}

impl DownloadArrivals {
    pub fn subscribe(&self, listener: ArrivalListener) {
        self.listeners.lock().push(listener);
    }

    /// A file for the song `key` is now at `path`. Listeners are called on this thread, so
    /// each only notes it down.
    pub fn arrived(&self, key: &str, path: &str) {
        let listeners = self.listeners.lock().clone();
        for listener in listeners {
            listener(key, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listener_hears_each_arrival() {
        let arrivals = DownloadArrivals::default();
        let heard = Arc::new(Mutex::new(Vec::new()));
        for name in ["a", "b"] {
            let heard = heard.clone();
            arrivals.subscribe(Arc::new(move |key, path| {
                heard.lock().push(format!("{name} {key} {path}"))
            }));
        }
        arrivals.arrived("youtube:abc", "/music/A/B/01 C.mp3");
        assert_eq!(
            *heard.lock(),
            [
                "a youtube:abc /music/A/B/01 C.mp3",
                "b youtube:abc /music/A/B/01 C.mp3"
            ]
        );
    }
}
