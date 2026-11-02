//! The poll thread's side of the spec thread: which specs are loaded, in a
//! least-recently-used list of [`CAPACITY`] names. A spec leaving the list is
//! dropped on the spec thread too, which frees its JavaScript context.
//!
//! The list is a hand-written `VecDeque`, most recent at the back: with 32
//! entries a linear search costs less than any map.
//!
//! A user spec edited, added or removed is seen at the next prompt: the
//! first time a name is asked for after a prompt (OSC 133;A, see
//! [`SpecCache::on_prompt`]) a [`Job::Recheck`] goes to the spec thread,
//! which compares the files' modification times and sizes with what it
//! loaded and, when they changed, loads the spec again and answers with
//! [`Outcome::Changed`]. No file is looked at on this side.
//!
//! It also sends the function calls the parse needs ([`SpecCache::call`]),
//! each with its own id, and keeps their answers until they are taken; and
//! sends other jobs ([`SpecCache::send`]: generators, usage saves), keeping
//! the generators' answers for the poll side's store ([`GenAnswer`]).

use std::collections::{HashMap, VecDeque};
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::generators::GenError;
use super::model::Suggestion;
use super::runtime::{CallError, Env, Job, LoadError, LoadedSpec, Outcome, SpecThread};
use crate::guards::ShellKind;

/// A spec whose load ran out of time is asked for again after this long; a
/// busy machine should not turn a spec off for the whole session.
const RETRY_AFTER_TIMEOUT: Duration = Duration::from_secs(30);

/// How many specs stay loaded.
pub const CAPACITY: usize = 32;

/// What [`SpecCache::prewarm`] loads.
pub const PREWARM: [&str; 6] = ["git", "cd", "ls", "npm", "docker", "brew"];

/// A spec as the poll thread sees it.
#[derive(Debug, Clone)]
pub enum SpecState {
    Ready(Arc<LoadedSpec>),
    /// Asked for; not back yet.
    Loading,
    /// It did not load; it is not asked for again while it stays in the
    /// list.
    Failed,
    /// There is no spec thread (it did not start, or it died).
    Unavailable,
}

#[derive(Debug)]
enum State {
    Loading,
    Ready(Arc<LoadedSpec>),
    /// When to try again (only after a timeout).
    Failed(Option<Instant>),
}

/// The calls answered, by call id.
pub type Called = HashMap<u64, Result<String, CallError>>;

/// A generator's answer, or a [`Job::Terms`] answer, for the poll side's
/// generator store (`super::generated`).
#[derive(Debug)]
pub enum GenAnswer {
    Generated {
        id: u64,
        result: Result<Vec<Suggestion>, GenError>,
        stale: bool,
    },
    Terms {
        id: u64,
        trigger: Option<bool>,
        query_term: Option<String>,
    },
    /// The spec's context changed (it loaded again, failed, or left the
    /// list): the function ids its old context gave are not callable.
    Reloaded { spec: String },
    /// These function ids of the spec's (walked in an answer the poll side
    /// let go of) are not callable any more.
    Released { spec: String, ids: Vec<u32> },
}

/// A function call for [`SpecCache::call`].
#[derive(Debug, Clone)]
pub struct Call {
    pub spec: String,
    /// The spec's load whose function `fn_id` is.
    pub generation: u64,
    pub fn_id: u32,
    /// A JSON array.
    pub args_json: String,
    pub cwd: Option<PathBuf>,
    pub env: Option<Env>,
    /// Hand the function `executeShellCommand` as its last argument.
    pub with_exec: bool,
    pub budget: Duration,
}

/// A name in the list: its state, and the prompt it was last rechecked at.
type Slot = (String, State, u64);

pub struct SpecCache {
    thread: Option<SpecThread>,
    lru: VecDeque<Slot>,
    /// How many prompts there were; see [`SpecCache::on_prompt`].
    prompt: u64,
    called: Called,
    /// Generator and term answers not taken yet.
    answers: Vec<GenAnswer>,
    /// The last call id given out.
    calls: u64,
    /// Names whose state changed (loaded, replaced, failed or evicted)
    /// since the last [`SpecCache::take_changed`].
    changed: Vec<String>,
    /// The same names, since the last [`SpecCache::take_reloaded`].
    reloaded: Vec<String>,
}

impl SpecCache {
    /// Start the spec thread for a `shell` session. Without one, every
    /// spec is `Unavailable`.
    pub fn new(shell: ShellKind) -> Self {
        let thread = SpecThread::start(shell)
            .inspect_err(|err| tracing::warn!("no spec thread ({err:#}); running without specs"))
            .ok();
        Self::with_thread(thread)
    }

    pub fn with_thread(thread: Option<SpecThread>) -> Self {
        Self {
            thread,
            lru: VecDeque::with_capacity(CAPACITY + 1),
            prompt: 0,
            called: HashMap::new(),
            answers: Vec::new(),
            calls: 0,
            changed: Vec::new(),
            reloaded: Vec::new(),
        }
    }

    fn live(&self) -> Option<&SpecThread> {
        self.thread.as_ref().filter(|thread| !thread.is_dead())
    }

    /// The result pipe to poll, while the thread lives.
    pub fn result_fd(&self) -> Option<RawFd> {
        self.live().map(SpecThread::result_fd)
    }

    /// What the thread has answered since the last call.
    pub fn take(&mut self) -> Vec<Outcome> {
        self.thread
            .as_mut()
            .map(SpecThread::take)
            .unwrap_or_default()
    }

    /// A prompt started (OSC 133;A): each name is rechecked for a changed
    /// user spec the next time it is asked for.
    pub fn on_prompt(&mut self) {
        self.prompt += 1;
    }

    /// A spec's state; the first ask loads it, and the first ask after a
    /// prompt has its user spec files rechecked on the spec thread.
    pub fn get(&mut self, name: &str) -> SpecState {
        let Some(thread) = self.thread.as_ref().filter(|thread| !thread.is_dead()) else {
            return SpecState::Unavailable;
        };
        if let Some(at) = self.lru.iter().position(|(known, ..)| known == name) {
            let entry = self.lru.remove(at);
            self.lru.extend(entry);
            if let Some((_, state, checked)) = self.lru.back_mut() {
                if let State::Failed(Some(retry)) = state
                    && Instant::now() >= *retry
                {
                    tracing::debug!("spec load name={name} (retry)");
                    thread.send(Job::Load {
                        name: name.to_owned(),
                        cwd: None,
                        env: None,
                    });
                    *state = State::Loading;
                    *checked = self.prompt;
                } else if *checked < self.prompt && !matches!(state, State::Loading) {
                    thread.send(Job::Recheck {
                        name: name.to_owned(),
                    });
                    *checked = self.prompt;
                }
            }
            return match self.lru.back() {
                Some((_, State::Ready(spec), _)) => SpecState::Ready(Arc::clone(spec)),
                Some((_, State::Failed(_), _)) => SpecState::Failed,
                Some((_, State::Loading, _)) | None => SpecState::Loading,
            };
        }
        let evicted = if self.lru.len() >= CAPACITY {
            self.lru.pop_front().map(|(evicted, ..)| evicted)
        } else {
            None
        };
        if let Some(evicted) = &evicted {
            thread.send(Job::Drop {
                name: evicted.clone(),
            });
        }
        tracing::debug!("spec load name={name}");
        thread.send(Job::Load {
            name: name.to_owned(),
            cwd: None,
            env: None,
        });
        self.lru
            .push_back((name.to_owned(), State::Loading, self.prompt));
        if let Some(evicted) = evicted {
            self.state_changed(evicted);
        }
        SpecState::Loading
    }

    /// A spec's state as it is, without loading it or moving it in the
    /// list; `None` when it is not in the list.
    pub fn peek(&self, name: &str) -> Option<SpecState> {
        if self.live().is_none() {
            return Some(SpecState::Unavailable);
        }
        let (_, state, _) = self.lru.iter().find(|(known, ..)| known == name)?;
        Some(match state {
            State::Ready(spec) => SpecState::Ready(Arc::clone(spec)),
            State::Failed(_) => SpecState::Failed,
            State::Loading => SpecState::Loading,
        })
    }

    /// Send a call; its answer is in [`SpecCache::take_called`] under the
    /// id returned. `None` without a live thread.
    pub fn call(&mut self, call: Call) -> Option<u64> {
        let thread = self.live()?;
        let id = self.calls + 1;
        tracing::debug!(
            "spec call id={id} spec={} fn={} exec={}",
            call.spec,
            call.fn_id,
            call.with_exec
        );
        thread.send(Job::Call {
            id,
            spec: call.spec,
            generation: call.generation,
            fn_id: call.fn_id,
            args_json: call.args_json,
            cwd: call.cwd,
            env: call.env,
            with_exec: call.with_exec,
            budget: call.budget,
        });
        self.calls = id;
        Some(id)
    }

    /// Take in the thread's answers: the names that became ready.
    pub fn on_outcomes(&mut self, outcomes: Vec<Outcome>) -> Vec<String> {
        let mut ready = Vec::new();
        for outcome in outcomes {
            match outcome {
                Outcome::Loaded { name, result } => {
                    // An evicted spec's late answer is dropped. A ready spec
                    // is sent again when its CLI's version is found to be
                    // another one; the new one replaces it.
                    let Some((_, state, _)) = self.lru.iter_mut().find(|(known, state, _)| {
                        *known == name
                            && (matches!(state, State::Loading)
                                || (matches!(state, State::Ready(_)) && result.is_ok()))
                    }) else {
                        continue;
                    };
                    match result {
                        Ok(spec) => {
                            tracing::debug!(
                                "spec ready name={name} ms={:.1}",
                                spec.load_time.as_secs_f64() * 1000.0
                            );
                            *state = State::Ready(spec);
                            self.state_changed(name.clone());
                            ready.push(name);
                        }
                        Err(err) => {
                            tracing::debug!("spec failed name={name}: {}", err.summary());
                            let retry = matches!(err, LoadError::Timeout)
                                .then(|| Instant::now() + RETRY_AFTER_TIMEOUT);
                            *state = State::Failed(retry);
                            self.state_changed(name);
                        }
                    }
                }
                Outcome::Changed { name, result } => {
                    // The user spec changed: what it is now replaces what it
                    // was, whatever that was. An evicted name is dropped.
                    let Some((_, state, _)) =
                        self.lru.iter_mut().find(|(known, ..)| *known == name)
                    else {
                        continue;
                    };
                    match result {
                        Ok(spec) => {
                            tracing::debug!("spec changed name={name}");
                            *state = State::Ready(spec);
                            self.state_changed(name.clone());
                            ready.push(name);
                        }
                        Err(err) => {
                            tracing::debug!("spec changed name={name}, failed: {}", err.summary());
                            *state = State::Failed(None);
                            self.state_changed(name);
                        }
                    }
                }
                Outcome::Called { id, result, .. } => {
                    match &result {
                        Ok(_) => tracing::debug!("spec called id={id}"),
                        Err(err) => tracing::debug!("spec called id={id} failed: {err}"),
                    }
                    self.called.insert(id, result);
                }
                Outcome::Generated { id, result, stale } => {
                    self.answers
                        .push(GenAnswer::Generated { id, result, stale });
                }
                Outcome::Terms {
                    id,
                    trigger,
                    query_term,
                } => self.answers.push(GenAnswer::Terms {
                    id,
                    trigger,
                    query_term,
                }),
                Outcome::Dead => {
                    tracing::error!("the spec thread died; specs are off for this session");
                    let gone: Vec<String> = self.lru.drain(..).map(|(name, ..)| name).collect();
                    for name in gone {
                        self.state_changed(name);
                    }
                }
                #[cfg(test)]
                _ => {}
            }
        }
        ready
    }

    /// Send a job; it is given back when there is no live thread.
    pub fn send(&mut self, job: Job) -> Option<Job> {
        match self.live() {
            Some(thread) => thread.try_send(job),
            None => Some(job),
        }
    }

    /// The poll side let go of an answer of `spec`'s that held the walked
    /// functions `ids`: the spec thread forgets them, and the generator
    /// store hears of it ([`GenAnswer::Released`]).
    pub fn release(&mut self, spec: String, ids: Vec<u32>) {
        tracing::debug!("spec release spec={spec} n={}", ids.len());
        if let Some(thread) = self.live() {
            thread.send(Job::Release {
                spec: spec.clone(),
                ids: ids.clone(),
            });
        }
        self.answers.push(GenAnswer::Released { spec, ids });
    }

    /// The generator and term answers since the last take.
    pub fn take_gen_answers(&mut self) -> Vec<GenAnswer> {
        std::mem::take(&mut self.answers)
    }

    /// The calls answered since the last take.
    pub fn take_called(&mut self) -> Called {
        std::mem::take(&mut self.called)
    }

    /// The names whose state changed since the last take.
    pub fn take_changed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.changed)
    }

    /// The names whose state changed since the last take, for the needs'
    /// answers (`super::needs`); the generator store hears of them as
    /// [`GenAnswer::Reloaded`].
    pub fn take_reloaded(&mut self) -> Vec<String> {
        std::mem::take(&mut self.reloaded)
    }

    /// A spec's state changed, so its context did (or it has none now).
    fn state_changed(&mut self, name: String) {
        self.reloaded.push(name.clone());
        self.answers
            .push(GenAnswer::Reloaded { spec: name.clone() });
        self.changed.push(name);
    }

    /// Load the specs most likely to be needed first.
    pub fn prewarm(&mut self) {
        for name in PREWARM {
            self.get(name);
        }
    }

    /// Stop the thread (waiting at most 50 ms); every spec is `Unavailable`
    /// after this.
    pub fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            thread.shutdown();
        }
        self.lru.clear();
    }

    #[cfg(test)]
    pub(crate) fn thread_mut(&mut self) -> Option<&mut SpecThread> {
        self.thread.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::specs::archive::Archive;
    use crate::specs::runtime::tests::{TEST_BUDGET, specs_with_functions, wait_readable};

    const WAIT: Duration = Duration::from_secs(60);

    fn cache() -> SpecCache {
        SpecCache::with_thread(Some(
            SpecThread::start_with_budget(None, TEST_BUDGET).expect("spec thread"),
        ))
    }

    /// Take in outcomes until `name` is no longer loading.
    fn settled(cache: &mut SpecCache, name: &str) -> SpecState {
        let deadline = Instant::now() + WAIT;
        loop {
            match cache.get(name) {
                SpecState::Loading => {}
                state => return state,
            }
            assert!(Instant::now() < deadline, "{name} still loading");
            let fd = cache.result_fd().expect("a live thread");
            wait_readable(fd);
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
        }
    }

    /// The runtime's heap, after a GC; other outcomes are taken in.
    fn memory(cache: &mut SpecCache) -> i64 {
        let deadline = Instant::now() + WAIT;
        let thread = cache.thread_mut().expect("a thread");
        thread.send(Job::MemoryUsage);
        loop {
            assert!(Instant::now() < deadline, "no memory usage answer");
            let thread = cache.thread_mut().expect("a thread");
            wait_readable(thread.result_fd());
            let mut usage = None;
            let mut others = Vec::new();
            for outcome in thread.take() {
                match outcome {
                    Outcome::MemoryUsage(bytes) => usage = Some(bytes),
                    other => others.push(other),
                }
            }
            cache.on_outcomes(others);
            if let Some(bytes) = usage {
                return bytes;
            }
        }
    }

    #[test]
    fn a_miss_loads_once_and_then_is_ready() {
        let mut cache = cache();
        assert!(matches!(cache.get("ls"), SpecState::Loading));
        assert!(matches!(cache.get("ls"), SpecState::Loading));
        let fd = cache.result_fd().expect("a live thread");
        wait_readable(fd);
        let mut outcomes = cache.take();
        // Only one load was sent.
        std::thread::sleep(Duration::from_millis(50));
        outcomes.extend(cache.take());
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(cache.on_outcomes(outcomes), ["ls"]);
        let SpecState::Ready(ls) = cache.get("ls") else {
            panic!("ls is not ready");
        };
        assert_eq!(ls.name, "ls");
        assert!(matches!(
            settled(&mut cache, "nonexistent"),
            SpecState::Failed
        ));
        cache.shutdown();
        assert!(matches!(cache.get("ls"), SpecState::Unavailable));
    }

    #[test]
    fn prewarm_loads_the_common_specs() {
        let mut cache = cache();
        cache.prewarm();
        for name in PREWARM {
            assert!(
                matches!(settled(&mut cache, name), SpecState::Ready(_)),
                "{name}"
            );
        }
    }

    #[test]
    fn the_33rd_spec_evicts_the_first_and_frees_its_context() {
        let archive = Archive::embedded();
        let mut small: Vec<(usize, &str)> = specs_with_functions()
            .into_iter()
            .filter(|name| *name != "aws/ec2")
            .filter_map(|name| Some((archive.source(name)?.ok()?.len(), name)))
            .collect();
        small.sort();
        let mut cache = cache();
        let first = "aws/ec2";
        assert!(matches!(settled(&mut cache, first), SpecState::Ready(spec) if spec.has_js));
        for &(_, name) in &small[..CAPACITY - 1] {
            assert!(
                matches!(settled(&mut cache, name), SpecState::Ready(spec) if spec.has_js),
                "{name}"
            );
        }
        let full = memory(&mut cache);
        let (_, last) = small[CAPACITY - 1];
        assert!(matches!(settled(&mut cache, last), SpecState::Ready(_)));
        assert_eq!(cache.lru.len(), CAPACITY);
        assert!(
            cache.lru.iter().all(|(name, ..)| name != first),
            "{first} was evicted"
        );
        let after = memory(&mut cache);
        assert!(after < full, "{full} -> {after}");
        assert!(
            full - after > 1 << 20,
            "dropping {first} freed only {} bytes",
            full - after
        );
    }

    #[test]
    fn a_dead_thread_makes_every_spec_unavailable() {
        let mut cache = cache();
        assert!(matches!(settled(&mut cache, "ls"), SpecState::Ready(_)));
        cache.thread_mut().expect("a thread").send(Job::Panic);
        let fd = cache.result_fd().expect("a live thread");
        wait_readable(fd);
        let outcomes = cache.take();
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, Outcome::Dead))
        );
        cache.on_outcomes(outcomes);
        assert!(cache.result_fd().is_none());
        assert!(matches!(cache.get("ls"), SpecState::Unavailable));
        assert!(matches!(cache.get("git"), SpecState::Unavailable));
    }

    #[test]
    fn no_thread_means_unavailable() {
        let mut cache = SpecCache::with_thread(None);
        assert!(cache.result_fd().is_none());
        assert!(matches!(cache.get("git"), SpecState::Unavailable));
        cache.prewarm();
        assert!(cache.take().is_empty());
        assert!(cache.on_outcomes(Vec::new()).is_empty());
        assert!(cache.take_called().is_empty());
    }

    /// A user spec rewritten after it was used shows its new items once a
    /// prompt has passed, not before; removed, fac's own spec comes back.
    #[test]
    fn a_user_spec_edit_shows_after_the_next_prompt() {
        let home = std::env::temp_dir().join(format!("fac-cache-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let dir = crate::specs::user::dir(&home);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("git.js");
        let spec = |sub: &str| {
            format!(r#"export default {{ name: "git", subcommands: [{{ name: "{sub}" }}] }};"#)
        };
        std::fs::write(&file, spec("mine")).expect("write");
        let mut cache = SpecCache::with_thread(Some(
            SpecThread::start_with_budget(Some(home.clone()), TEST_BUDGET).expect("spec thread"),
        ));
        let first = |cache: &mut SpecCache| match settled(cache, "git") {
            SpecState::Ready(spec) => spec.root.subcommands[0].name[0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(first(&mut cache), "mine");
        std::fs::write(&file, spec("edited")).expect("rewrite");
        // No prompt yet: nothing is rechecked.
        std::thread::sleep(Duration::from_millis(50));
        let outcomes = cache.take();
        assert!(cache.on_outcomes(outcomes).is_empty());
        assert_eq!(first(&mut cache), "mine");
        cache.on_prompt();
        assert!(
            matches!(cache.get("git"), SpecState::Ready(_)),
            "the old one meanwhile"
        );
        let deadline = Instant::now() + WAIT;
        while first(&mut cache) != "edited" {
            assert!(Instant::now() < deadline, "the edit never showed");
            let fd = cache.result_fd().expect("a live thread");
            wait_readable(fd);
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
        }
        assert!(cache.take_changed().contains(&"git".to_owned()));
        std::fs::remove_file(&file).expect("rm");
        cache.on_prompt();
        cache.get("git");
        let deadline = Instant::now() + WAIT;
        while first(&mut cache) == "edited" {
            assert!(Instant::now() < deadline, "fac's own git never came back");
            let fd = cache.result_fd().expect("a live thread");
            wait_readable(fd);
            let outcomes = cache.take();
            cache.on_outcomes(outcomes);
        }
        let SpecState::Ready(ours) = cache.get("git") else {
            panic!("git is not ready");
        };
        assert!(
            ours.root
                .subcommands
                .iter()
                .any(|sub| sub.name[0] == "commit")
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_second_loaded_replaces_a_ready_spec() {
        use crate::specs::model::Subcommand;
        use crate::specs::runtime::SpecVersion;

        let mut cache = cache();
        let SpecState::Ready(first) = settled(&mut cache, "ls") else {
            panic!("ls is not ready");
        };
        let again = Arc::new(LoadedSpec {
            name: "ls".to_owned(),
            root: Subcommand::default(),
            fns: Vec::new(),
            has_js: false,
            version: Some(SpecVersion::Detected("1.0.0".to_owned())),
            load_time: Duration::ZERO,
            generation: first.generation + 1,
        });
        let ready = cache.on_outcomes(vec![Outcome::Loaded {
            name: "ls".to_owned(),
            result: Ok(Arc::clone(&again)),
        }]);
        assert_eq!(ready, ["ls"]);
        let SpecState::Ready(now) = cache.get("ls") else {
            panic!("ls is not ready");
        };
        assert!(Arc::ptr_eq(&now, &again) && !Arc::ptr_eq(&now, &first));
        // A failed second load keeps the spec it has.
        cache.on_outcomes(vec![Outcome::Loaded {
            name: "ls".to_owned(),
            result: Err(LoadError::Timeout),
        }]);
        assert!(matches!(cache.get("ls"), SpecState::Ready(spec) if Arc::ptr_eq(&spec, &again)));
        // Nor is an evicted spec's late answer taken in.
        assert!(
            cache
                .on_outcomes(vec![Outcome::Loaded {
                    name: "cd".to_owned(),
                    result: Ok(again),
                }])
                .is_empty()
        );
    }
}
