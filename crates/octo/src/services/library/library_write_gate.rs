//! One writer at a time over the music folder.
//!
//! The jobs that rewrite many files (the library tidy, the genre re-tag, the explicit marking)
//! each take the gate whole: a second job is refused while one runs, and the dashboard says
//! which one. A download being placed and lyrics being written beside a song take it shared:
//! many at once, but never while a job holds it, so a file never lands in a folder that is
//! being moved or gets its tags written twice at once. A job waits for the placements already
//! under way to finish before it starts.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// The gate. Clones share it.
#[derive(Clone, Default)]
pub struct LibraryWriteGate {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    lock: Arc<RwLock<()>>,
    /// The job that has the gate, or is waiting for the placements under way to finish so it
    /// can take it, in the words the dashboard shows.
    job: Mutex<Option<JobSlot>>,
}

struct JobSlot {
    name: String,
    /// Whether the job has held the gate yet; until then it is not shown as writing.
    started: bool,
}

/// A job's hold on the gate. Dropping it lets the gate go.
pub struct LibraryJob {
    gate: LibraryWriteGate,
    write: Option<OwnedRwLockWriteGuard<()>>,
}

impl Drop for LibraryJob {
    fn drop(&mut self) {
        *self.gate.inner.job.lock() = None;
    }
}

impl LibraryJob {
    /// Lets placements and lyrics writes through while the job waits on something slow (a
    /// Navidrome scan). The job keeps its place: no other job starts meanwhile.
    pub fn pause(&mut self) {
        self.write = None;
    }

    /// Holds the gate whole again, once the placements let in meanwhile are done.
    pub async fn resume(&mut self) {
        if self.write.is_none() {
            self.write = Some(Arc::clone(&self.gate.inner.lock).write_owned().await);
        }
    }
}

/// Frees the job's place when it is given up before it holds the gate.
struct Waiting<'a> {
    gate: &'a LibraryWriteGate,
    armed: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.armed {
            *self.gate.inner.job.lock() = None;
        }
    }
}

/// A placement's or a lyrics write's share of the gate.
pub type LibraryWrite = OwnedRwLockReadGuard<()>;

impl LibraryWriteGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// The job writing to the library now, if any.
    pub fn job(&self) -> Option<String> {
        self.inner
            .job
            .lock()
            .as_ref()
            .filter(|slot| slot.started)
            .map(|slot| slot.name.clone())
    }

    /// The refusal for a job that would start while another holds the gate.
    pub fn busy_message(holder: &str) -> String {
        format!("{holder} is changing the music folder right now. Try again when it is done.")
    }

    /// Take the gate for a job named `name`. Refused, with the running job's name, when
    /// another job holds it; otherwise waits for placements under way and holds the gate.
    pub async fn begin_job(&self, name: &str) -> Result<LibraryJob, String> {
        {
            let mut job = self.inner.job.lock();
            if let Some(holder) = job.as_ref() {
                return Err(holder.name.clone());
            }
            *job = Some(JobSlot {
                name: name.to_string(),
                started: false,
            });
        }
        let mut waiting = Waiting {
            gate: self,
            armed: true,
        };
        let write = Arc::clone(&self.inner.lock).write_owned().await;
        waiting.armed = false;
        if let Some(slot) = self.inner.job.lock().as_mut() {
            slot.started = true;
        }
        Ok(LibraryJob {
            gate: self.clone(),
            write: Some(write),
        })
    }

    /// A share of the gate for one placement or one lyrics write: waits while a job holds it.
    pub async fn write(&self) -> LibraryWrite {
        Arc::clone(&self.inner.lock).read_owned().await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_second_job_is_refused_with_the_running_jobs_name() {
        let gate = LibraryWriteGate::new();
        let tidy = gate.begin_job("Tidy artists").await.expect("free");

        let second = tokio::time::timeout(Duration::from_secs(1), gate.begin_job("The genre re-tag"))
            .await
            .expect("refused at once rather than waiting");
        assert_eq!(second.err().as_deref(), Some("Tidy artists"));
        assert_eq!(gate.job().as_deref(), Some("Tidy artists"));

        drop(tidy);
        assert_eq!(gate.job(), None);
        assert!(gate.begin_job("The genre re-tag").await.is_ok());
    }

    #[tokio::test]
    async fn a_placement_waits_while_a_job_writes() {
        let gate = LibraryWriteGate::new();
        let job = gate.begin_job("Tidy artists").await.expect("free");

        let waiting = tokio::time::timeout(Duration::from_millis(30), gate.write()).await;
        assert!(waiting.is_err(), "a placement went ahead during a job");

        drop(job);
        let placed = tokio::time::timeout(Duration::from_secs(1), gate.write()).await;
        assert!(placed.is_ok());
    }

    #[tokio::test]
    async fn a_job_shows_as_writing_only_once_it_holds_the_gate() {
        let gate = LibraryWriteGate::new();
        let placing = gate.write().await;
        let other = gate.clone();
        let job = tokio::spawn(async move { other.begin_job("Tidy artists").await });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(gate.job(), None);
        // Its place is taken all the same.
        assert_eq!(
            gate.begin_job("The genre re-tag").await.err().as_deref(),
            Some("Tidy artists")
        );

        drop(placing);
        let job = job.await.expect("joined").expect("started");
        assert_eq!(gate.job().as_deref(), Some("Tidy artists"));
        drop(job);
    }

    #[tokio::test]
    async fn a_job_given_up_while_it_waits_leaves_the_gate_free() {
        let gate = LibraryWriteGate::new();
        let placing = gate.write().await;

        let given_up = tokio::time::timeout(Duration::from_millis(30), gate.begin_job("Tidy artists")).await;
        assert!(given_up.is_err(), "it waited for the placement");
        drop(placing);

        assert_eq!(gate.job(), None);
        let next = tokio::time::timeout(Duration::from_secs(1), gate.begin_job("The genre re-tag"))
            .await
            .expect("not waiting");
        assert!(next.is_ok(), "the gate was left taken");
    }

    #[tokio::test]
    async fn a_paused_job_lets_placements_through_and_keeps_its_place() {
        let gate = LibraryWriteGate::new();
        let mut job = gate.begin_job("Tidy artists").await.expect("free");
        job.pause();

        let placed = tokio::time::timeout(Duration::from_secs(1), gate.write())
            .await
            .expect("placed while the job waits");
        assert_eq!(gate.job().as_deref(), Some("Tidy artists"));
        assert!(gate.begin_job("The genre re-tag").await.is_err());

        let resuming = tokio::time::timeout(Duration::from_millis(30), job.resume()).await;
        assert!(resuming.is_err(), "resumed over a placement under way");
        drop(placed);
        tokio::time::timeout(Duration::from_secs(1), job.resume())
            .await
            .expect("resumed");
        let waiting = tokio::time::timeout(Duration::from_millis(30), gate.write()).await;
        assert!(waiting.is_err(), "a placement went ahead after the job resumed");
    }

    #[tokio::test]
    async fn a_job_waits_for_placements_under_way() {
        let gate = LibraryWriteGate::new();
        let placing = gate.write().await;
        let other = gate.clone();
        let job = tokio::spawn(async move { other.begin_job("Tidy artists").await.map(|_| ()) });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !job.is_finished(),
            "the job started while a download was being placed"
        );
        drop(placing);
        assert!(job.await.expect("joined").is_ok());
    }
}
