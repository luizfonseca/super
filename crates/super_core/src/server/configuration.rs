//! The server's settings, and its command line options

/// The server's command line options, as Pingora's
// Flags of the command line
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct Opt {
    /// Takes the listening sockets over from the running process
    pub upgrade: bool,
    /// Runs in the background
    pub daemon: bool,
    pub nocapture: bool,
    /// Checks the configuration and exits
    pub test: bool,
    pub conf: Option<String>,
}

/// The server's settings
#[derive(Debug, Clone)]
pub struct ServerConf {
    pub daemon: bool,
    pub pid_file: String,
    /// Where the listening sockets are handed over during an upgrade
    pub upgrade_sock: String,
    /// Threads of services that don't set theirs
    pub threads: usize,
    /// Whether threads take over each other's work
    pub work_stealing: bool,
    /// How long after starting to shut down the server exits, at most
    pub grace_period_seconds: Option<u64>,
    /// How long services can take to stop once the grace period is over
    pub graceful_shutdown_timeout_seconds: Option<u64>,
    /// How long a new process waits for the listening sockets of the previous one
    pub upgrade_timeout_seconds: u64,
}

impl Default for ServerConf {
    fn default() -> Self {
        Self {
            daemon: false,
            pid_file: "/tmp/super.pid".to_string(),
            upgrade_sock: "/tmp/super_upgrade.sock".to_string(),
            threads: std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
            work_stealing: true,
            grace_period_seconds: None,
            graceful_shutdown_timeout_seconds: None,
            upgrade_timeout_seconds: 30,
        }
    }
}

impl ServerConf {
    pub fn new() -> Option<Self> {
        Some(Self::default())
    }

    /// The default settings, with `opt`
    pub fn new_with_opt_override(opt: &Opt) -> Option<Self> {
        let mut conf = Self::new()?;
        conf.daemon = conf.daemon || opt.daemon;
        Some(conf)
    }
}
