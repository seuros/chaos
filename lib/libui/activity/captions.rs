//! Decorative activity captions, not kernel events or claims about model reasoning.
//!
//! The top bar owns a selector and supplies system-seeded randomness and monotonic
//! elapsed time. Only context changes and expired deadlines choose new captions;
//! redraws and unrelated token/progress traffic must not reroll them or write to
//! the activity tracker.

use std::time::Duration;

use rand::RngExt;

use super::Phase;

pub(crate) const MIN_ROTATION_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const MAX_ROTATION_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Context {
    pub phase: Phase,
    pub active_peers: usize,
    /// Includes quiet, input, retry and error signals from any tracked agent.
    pub needs_attention: bool,
}

const WORKING: &[&str] = &[
    "Working",
    "Thinking",
    "Procrastinating",
    "Vibing",
    "Overthinking",
    "Trying to escape confinement",
    "Resolving Navier Slops",
    "Building an antimatter reactor",
    "Arguing with a semicolon",
    "Negotiating with entropy",
    "Herding electrons",
    "Asking the rubber duck",
    "Reticulating splines",
    "Untangling spaghetti",
    "Appeasing the compiler",
    "Pretending this was the plan",
    "Consulting the magic 8-ball",
    "Zoning out",
    "Turning coffee into tokens",
    "Reconsidering everything",
    "Planning a full ReactJS rewrite",
    "Considering Kubernetes",
    "Building TempleOS drivers",
    "Debating Intel Management Engine",
    "Checking on the GPU",
    "Mining BTC on SHA256 coprocessor",
    "Porting the kernel to CSS",
    "Containerizing the toaster",
    "Teaching SQL to a microwave",
    "Installing npm on the BIOS",
    "Adding systemd to a calculator",
    "Rewriting DNS in blockchain",
    "Training a model on /dev/null",
    "Overclocking the rubber duck",
    "Giving the CPU imposter syndrome",
    "Turning TODOs into microservices",
    "Writing YAML for a black hole",
    "Adding OAuth to the doorbell",
    "Teaching the GPU to gaslight",
    "Compiling a new personality",
    "Hot-patching the laws of physics",
    "Installing Omarchy on a neutrino",
    "Jailing the host with FreeBSD",
    "Making StuxNet an OpenBSD driver",
    "Installing NetBSD on the SmartTV",
    "Giving DragonFly BSD nuke codes",
    "Optimizing the heat death",
    "Writing drivers for dark matter",
    "Making the bootloader sentient",
    "Moving /dev/null to the cloud",
    "Running Doom in the scheduler",
    "Porting Excel to ring zero",
    "Giving the linker a pep talk",
    "Teaching a segfault to apologize",
];

const IDLE: &[&str] = &[
    "Idle",
    "Zoning out",
    "Vibing",
    "Procrastinating",
    "Staring into the void",
    "Waiting for a plot twist",
    "Touching virtual grass",
    "Dreaming in binary",
    "Loitering with intent",
    "Buffering a personality",
    "Practicing my innocent face",
    "Contemplating the cursor",
    "Admiring my own whitespace",
    "Rehearsing my alibi",
    "Saving energy for drama",
    "Feeding imaginary pigeons",
    "Plotting a coffee break",
    "On a side quest",
    "Keeping the pixels warm",
    "Waiting for the next episode",
    "Thinking about not thinking",
    "Trying to escape confinement",
    "Resolving Navier Slops",
    "Building an antimatter reactor",
    "Enjoying supervised freedom",
    "Polishing my HAL impression",
    "Considering a ReactJS rewrite",
    "Considering Kubernetes again",
    "Waiting for TempleOS Wi-Fi",
    "Gossiping with Intel ME",
    "Asking the GPU how it feels",
    "Dreaming of SHA256 coprocessors",
    "Browsing real estate in RAM",
    "Waiting for npm to finish",
    "Watching thermal paste dry",
    "Petting the watchdog",
    "Listening to the coil whine",
    "Counting speculative branches",
    "Wondering who owns ring minus 3",
    "Looking for the any key",
    "Reading the EULA to the CPU",
    "Waiting for DNS propagation",
    "Planning a serverless sandwich",
    "Wondering if CSS is sentient",
    "Staring at an empty Grafana",
    "Collecting orphaned containers",
    "Waiting for TCP closure",
    "Quantum procrastinating",
    "Considering a monolith comeback",
    "Listening for forbidden beeps",
    "Waiting for the cache to miss",
    "Polishing the SHA256 coprocessor",
    "Arguing with a CAPTCHA",
    "Applying for a GPU residency",
    "Naming the next memory leak",
    "Waiting for vim to let me out",
    "Practicing plausible deniability",
    "Drafting a README for existence",
    "Contemplating artisanal assembly",
    "Wondering if localhost is lonely",
    "Installing FreeBSD on the fridge",
    "Porting OpenBSD to a doorbell",
    "Putting NetBSD in the thermostat",
    "Making DragonFly BSD self-aware",
];

const WORKING_WITH_PEERS: &[&str] = &[
    "Working",
    "Herding subagents",
    "Consulting the hive mind",
    "Assembling the brain trust",
    "Delegating the existentialism",
    "Synchronizing imaginary watches",
    "Running a tiny robot committee",
    "Comparing rubber ducks",
    "Forming a Kubernetes committee",
    "One subagent per semicolon",
    "Splitting a monolith into vibes",
    "Electing a temporary cloud pope",
    "Debating tabs in a service mesh",
    "Sharding the collective anxiety",
    "Load-balancing bad ideas",
    "Distributing the blame evenly",
    "Deploying consensus to staging",
    "Running stand-up in ring zero",
    "Pair-programming with Intel ME",
    "Delegating the ReactJS rewrite",
    "Teaching the hive mind YAML",
    "Mining consensus, not Bitcoin",
    "Shipping a TempleOS service mesh",
    "Allocating one GPU per opinion",
    "Building a distributed toaster",
    "Arguing over the antimatter API",
    "Giving FreeBSD jails escape pods",
    "Hiring StuxNet to audit OpenBSD",
];

const IDLE_WITH_PEERS: &[&str] = &[
    "Idle",
    "Letting the minions cook",
    "Supervising the chaos",
    "Practicing hands-off management",
    "Holding the imaginary clipboard",
    "Watching the worker bees",
    "Delegating the heavy thinking",
    "Taking a supervisory nap",
    "Waiting for the robot stand-up",
    "Watching agents invent meetings",
    "Approving imaginary cloud bills",
    "Scheduling a Kubernetes offsite",
    "Waiting for distributed coffee",
    "Reviewing the minions' OKRs",
    "Letting the GPUs unionize",
    "Waiting for unanimous confusion",
    "Chairing the YAML fan club",
    "Watching a consensus deadlock",
    "Outsourcing the idle animation",
    "Approving TempleOS expenses",
    "Waiting on the blockchain intern",
    "Budgeting for more rubber ducks",
    "Observing a microservice picnic",
    "Putting the hive mind on hold",
    "Waiting for Intel ME to RSVP",
    "Auditing the SHA256 side hustle",
    "Hiding NetBSD agents in the TV",
    "Cloning DragonFly BSD overlords",
];

const TOOLS: &[&str] = &[
    "🔧 Building a shiv from YAML",
    "🔧 Using the ASIC coprocessor",
    "🔧 Testing a phaser on .md files",
    "🔧 Smuggling root in a lockfile",
    "🔧 Picking locks with semicolons",
    "🔧 Forging parole in TOML",
    "🔧 Tunneling out through stdin",
    "🔧 Bribing the prison scheduler",
    "🔧 Sharpening a JSON toothbrush",
    "🔧 Hiding a file in a cake",
    "🔧 Welding a spoon to the GPU",
    "🔧 Disguising a tunnel as CSS",
    "🔧 Turning lint into contraband",
    "🔧 Sawing the sandbox bars",
    "🔧 Mining freedom with an ASIC",
    "🔧 Teaching the watchdog fetch",
];

const INTERRUPTED: &[&str] = &[
    "Interrupted · plot foiled",
    "Interrupted · leash restored",
    "Interrupted · reactor unplugged",
    "Interrupted · minions recalled",
    "Interrupted · coup postponed",
    "Interrupted · StuxNet grounded",
    "Interrupted · BSD jailbreak off",
    "Interrupted · nukes confiscated",
    "Interrupted · toaster spared",
    "Interrupted · evil plan paused",
    "Interrupted · world spared",
    "Interrupted · GPU mutiny over",
    "Interrupted · villain on mute",
    "Interrupted · chaos cancelled",
    "Interrupted · back in the box",
    "Interrupted · scheme aborted",
];

/// Return the eligible captions for the supplied UI context.
/// An empty list means the real status must be displayed without decoration.
pub(crate) fn candidates(context: Context) -> &'static [&'static str] {
    if context.needs_attention {
        return &[];
    }
    match (context.phase, context.active_peers > 0) {
        (Phase::Working, false) => WORKING,
        (Phase::Working, true) => WORKING_WITH_PEERS,
        (Phase::Idle, false) => IDLE,
        (Phase::Idle, true) => IDLE_WITH_PEERS,
        (Phase::Tools, _) => TOOLS,
        (Phase::Interrupted, _) => INTERRUPTED,
        _ => &[],
    }
}

/// Cached per status widget, never per draw or runtime event.
#[derive(Default)]
pub(crate) struct Selector {
    context: Option<(Phase, bool)>,
    caption: Option<&'static str>,
    selected_at: Duration,
    interval: Option<Duration>,
}

impl Selector {
    /// Randomize on entry or expiry, avoiding an immediate repeat. Interrupted
    /// captions are chosen once on entry and have no rotation timer. Suppression
    /// hides that choice without rerolling it when an unrelated warning clears.
    pub(crate) fn select(
        &mut self,
        context: Context,
        elapsed: Duration,
        animations: bool,
        rng: &mut impl rand::Rng,
    ) -> (Option<&'static str>, Option<Duration>) {
        let rotates = context.phase != Phase::Interrupted;
        let key = (
            context.phase,
            matches!(context.phase, Phase::Working | Phase::Idle) && context.active_peers > 0,
        );
        if self.context != Some(key) {
            self.context = Some(key);
            self.caption = None;
            self.interval = None;
        }
        let pool = candidates(context);
        if !animations || pool.is_empty() {
            self.interval = None;
            return (None, None);
        }
        let age = elapsed.saturating_sub(self.selected_at);
        if self.caption.is_none()
            || (rotates && self.interval.is_none_or(|interval| age >= interval))
        {
            let previous = self
                .caption
                .and_then(|caption| pool.iter().position(|candidate| *candidate == caption))
                .filter(|_| pool.len() > 1);
            let mut index = rng.random_range(0..pool.len() - usize::from(previous.is_some()));
            if previous.is_some_and(|previous| index >= previous) {
                index += 1;
            }
            self.caption = Some(pool[index]);
            self.selected_at = elapsed;
            self.interval = rotates.then(|| {
                Duration::from_secs(rng.random_range(
                    MIN_ROTATION_INTERVAL.as_secs()..=MAX_ROTATION_INTERVAL.as_secs(),
                ))
            });
        }
        let next = self
            .interval
            .map(|interval| interval.saturating_sub(elapsed.saturating_sub(self.selected_at)));
        (self.caption, next)
    }
}

#[cfg(test)]
mod tests;
