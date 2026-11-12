//! The A/B release transaction of OpenShell Native (ARCHITECTURE.md, section 6).
//!
//! Two system slots hold a release each. A selector, stored twice with a
//! generation and a checksum, records the state of each slot. An update
//! writes the inactive slot, verifies it against the release manifest and
//! asks for a trial boot. The boot step records each attempt before the
//! handover; a trial that exhausts its attempts falls back to the last
//! confirmed slot, and no valid slot means the recovery environment. The
//! running system confirms its slot after its health checks.
//!
//! Every change writes one selector copy, the older one, so that a torn
//! write leaves the newer copy intact.

pub mod sha256;

use std::fmt;

/// Boot attempts of a release on trial (ARCHITECTURE.md proposes three).
pub const TRIAL_BOOTS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Self {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "a" => Some(Slot::A),
            "b" => Some(Slot::B),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing bootable, or a release being written.
    Empty,
    /// Written and verified, not yet booted.
    Staged,
    /// Booting on trial, with the attempts left.
    Trial(u8),
    /// Confirmed by a running system.
    Good,
    /// Exhausted its trial or was rejected.
    Bad,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRecord {
    pub state: State,
    pub release: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    pub generation: u64,
    pub slots: [SlotRecord; 2],
    /// The slot to boot when no trial is pending.
    pub confirmed: Option<Slot>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Boot {
    Slot(Slot),
    Recovery,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// No valid selector copy: only the recovery environment may run.
    NoSelector,
    /// The operation does not apply to the slot's state.
    State(&'static str),
    Manifest(String),
    Storage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoSelector => write!(formatter, "no valid selector"),
            Error::State(message) => write!(formatter, "{message}"),
            Error::Manifest(message) => write!(formatter, "manifest: {message}"),
            Error::Storage(message) => write!(formatter, "storage: {message}"),
        }
    }
}

impl std::error::Error for Error {}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn valid_release(release: &str) -> bool {
    !release.is_empty()
        && release.len() <= 64
        && release.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte))
}

impl State {
    fn encode(&self) -> String {
        match self {
            State::Empty => "empty".into(),
            State::Staged => "staged".into(),
            State::Trial(tries) => format!("trial:{tries}"),
            State::Good => "good".into(),
            State::Bad => "bad".into(),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        Some(match text {
            "empty" => State::Empty,
            "staged" => State::Staged,
            "good" => State::Good,
            "bad" => State::Bad,
            _ => State::Trial(text.strip_prefix("trial:")?.parse().ok().filter(|tries| *tries <= TRIAL_BOOTS)?),
        })
    }
}

impl Selector {
    /// The selector of a new installation, with its first release in slot A.
    pub fn install(release: &str) -> Result<Self, Error> {
        if !valid_release(release) {
            return Err(Error::Manifest("invalid release name".into()));
        }
        Ok(Self {
            generation: 1,
            slots: [
                SlotRecord { state: State::Good, release: release.into() },
                SlotRecord { state: State::Empty, release: "-".into() },
            ],
            confirmed: Some(Slot::A),
        })
    }

    pub fn slot(&self, slot: Slot) -> &SlotRecord {
        &self.slots[slot.index()]
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut SlotRecord {
        &mut self.slots[slot.index()]
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut text = format!("openshell-ab 1\ngeneration={}\n", self.generation);
        for slot in [Slot::A, Slot::B] {
            let record = self.slot(slot);
            text += &format!("{}={} {}\n", slot.name(), record.state.encode(), record.release);
        }
        text += &format!("confirmed={}\n", self.confirmed.map_or("-", Slot::name));
        let crc = crc32(text.as_bytes());
        text += &format!("crc={crc:08x}\n");
        text.into_bytes()
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(data).ok()?;
        let (body, trailer) = text.rsplit_once("crc=")?;
        let crc = u32::from_str_radix(trailer.strip_suffix('\n')?, 16).ok()?;
        if crc32(body.as_bytes()) != crc {
            return None;
        }
        let mut lines = body.lines();
        if lines.next()? != "openshell-ab 1" {
            return None;
        }
        let generation = lines.next()?.strip_prefix("generation=")?.parse().ok()?;
        let mut slots = Vec::new();
        for slot in [Slot::A, Slot::B] {
            let (state, release) = lines.next()?.strip_prefix(slot.name())?.strip_prefix('=')?.split_once(' ')?;
            if release != "-" && !valid_release(release) {
                return None;
            }
            slots.push(SlotRecord { state: State::decode(state)?, release: release.into() });
        }
        let confirmed = match lines.next()?.strip_prefix("confirmed=")? {
            "-" => None,
            name => Some(Slot::parse(name)?),
        };
        if lines.next().is_some() {
            return None;
        }
        let slots: [SlotRecord; 2] = slots.try_into().ok()?;
        Some(Self { generation, slots, confirmed })
    }
}

/// The two copies of the selector on durable storage. A write replaces one
/// copy and returns once it is durable; a failure may leave that copy torn.
pub trait Store {
    fn read(&self, copy: usize) -> Option<Vec<u8>>;
    fn write(&mut self, copy: usize, data: &[u8]) -> Result<(), Error>;
}

/// The newest valid copy.
pub fn load(store: &impl Store) -> Option<Selector> {
    (0..2)
        .filter_map(|copy| Selector::decode(&store.read(copy)?))
        .max_by_key(|selector| selector.generation)
}

/// Writes the next generation into the copy that does not hold the newest one.
fn save(store: &mut impl Store, selector: &mut Selector) -> Result<(), Error> {
    selector.generation += 1;
    store.write((selector.generation % 2) as usize, &selector.encode())
}

/// Starts writing a release into the slot that is not confirmed. The slot
/// stops being bootable before its contents change.
pub fn begin_update(store: &mut impl Store) -> Result<Slot, Error> {
    let mut selector = load(store).ok_or(Error::NoSelector)?;
    let target = selector.confirmed.ok_or(Error::State("no confirmed slot to update from"))?.other();
    if matches!(selector.slot(target).state, State::Trial(_)) {
        return Err(Error::State("a trial is pending"));
    }
    *selector.slot_mut(target) = SlotRecord { state: State::Empty, release: "-".into() };
    save(store, &mut selector)?;
    Ok(target)
}

/// Records a written and verified release in the slot from `begin_update`.
pub fn finish_update(store: &mut impl Store, slot: Slot, release: &str) -> Result<(), Error> {
    let mut selector = load(store).ok_or(Error::NoSelector)?;
    if selector.slot(slot).state != State::Empty || selector.confirmed == Some(slot) || !valid_release(release) {
        return Err(Error::State("the slot was not prepared for this update"));
    }
    *selector.slot_mut(slot) = SlotRecord { state: State::Staged, release: release.into() };
    save(store, &mut selector)
}

/// Asks for trial boots of a staged release.
pub fn request_trial(store: &mut impl Store, slot: Slot) -> Result<(), Error> {
    let mut selector = load(store).ok_or(Error::NoSelector)?;
    if selector.slot(slot).state != State::Staged {
        return Err(Error::State("only a staged release goes on trial"));
    }
    selector.slot_mut(slot).state = State::Trial(TRIAL_BOOTS);
    save(store, &mut selector)
}

/// The boot step: chooses the slot and records a trial attempt before the
/// handover. A trial without attempts left becomes bad.
pub fn choose_boot(store: &mut impl Store) -> Result<Boot, Error> {
    let Some(mut selector) = load(store) else { return Ok(Boot::Recovery) };
    for slot in [Slot::A, Slot::B] {
        if let State::Trial(tries) = selector.slot(slot).state {
            if tries > 0 {
                selector.slot_mut(slot).state = State::Trial(tries - 1);
                save(store, &mut selector)?;
                return Ok(Boot::Slot(slot));
            }
            selector.slot_mut(slot).state = State::Bad;
            save(store, &mut selector)?;
        }
    }
    Ok(match selector.confirmed {
        Some(slot) if selector.slot(slot).state == State::Good => Boot::Slot(slot),
        _ => Boot::Recovery,
    })
}

/// Confirms the running slot after the health checks of the session.
pub fn confirm(store: &mut impl Store, running: Slot) -> Result<(), Error> {
    let mut selector = load(store).ok_or(Error::NoSelector)?;
    match selector.slot(running).state {
        State::Good if selector.confirmed == Some(running) => return Ok(()),
        State::Trial(_) | State::Good => {}
        _ => return Err(Error::State("the running slot cannot be confirmed")),
    }
    selector.slot_mut(running).state = State::Good;
    selector.confirmed = Some(running);
    save(store, &mut selector)
}

/// A release manifest: its identity, the range of data schemas it reads and
/// the size and SHA-256 of each artifact.
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    pub release: String,
    pub arch: String,
    pub schema: (u32, u32),
    pub files: Vec<(String, u64, [u8; 32])>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let fail = |message: &str| Error::Manifest(message.into());
        let (mut release, mut arch, mut schema) = (None, None, None);
        let mut files = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let (key, value) = line.split_once('=').ok_or_else(|| fail("expected key=value"))?;
            match key {
                "release" if valid_release(value) => release = Some(value.to_owned()),
                "arch" => arch = Some(value.to_owned()),
                "schema" => {
                    let (low, high) = value.split_once("..").ok_or_else(|| fail("schema is low..high"))?;
                    let range: (u32, u32) = (low.parse().map_err(|_| fail("schema"))?, high.parse().map_err(|_| fail("schema"))?);
                    if range.0 > range.1 {
                        return Err(fail("empty schema range"));
                    }
                    schema = Some(range);
                }
                "file" => {
                    let mut words = value.split(' ');
                    let (Some(name), Some(size), Some(digest), None) = (words.next(), words.next(), words.next(), words.next()) else {
                        return Err(fail("file is name size sha256"));
                    };
                    if name.is_empty() || name.contains('/') || name.starts_with('.') || files.iter().any(|(other, _, _)| other == name) {
                        return Err(fail("invalid file name"));
                    }
                    if digest.len() != 64 {
                        return Err(fail("invalid sha256"));
                    }
                    let mut bytes = [0u8; 32];
                    for (index, byte) in bytes.iter_mut().enumerate() {
                        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16).map_err(|_| fail("invalid sha256"))?;
                    }
                    files.push((name.to_owned(), size.parse().map_err(|_| fail("invalid size"))?, bytes));
                }
                _ => return Err(fail(&format!("unknown or invalid {key}"))),
            }
        }
        if files.is_empty() {
            return Err(fail("no files"));
        }
        Ok(Self {
            release: release.ok_or_else(|| fail("missing release"))?,
            arch: arch.ok_or_else(|| fail("missing arch"))?,
            schema: schema.ok_or_else(|| fail("missing schema"))?,
            files,
        })
    }

    /// Whether this computer can run the release with its current data.
    pub fn check_compatible(&self, arch: &str, data_schema: u32) -> Result<(), Error> {
        if self.arch != arch {
            return Err(Error::Manifest(format!("release for {}, not {arch}", self.arch)));
        }
        if !(self.schema.0..=self.schema.1).contains(&data_schema) {
            return Err(Error::Manifest(format!("data schema {data_schema} is outside the release's range")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Storage whose writes fail after a budget, leaving the copy torn: a
    /// power cut at that write.
    #[derive(Clone, Default)]
    struct Disk {
        copies: [Option<Vec<u8>>; 2],
        writes_left: Option<usize>,
    }

    impl Store for Disk {
        fn read(&self, copy: usize) -> Option<Vec<u8>> {
            self.copies[copy].clone()
        }

        fn write(&mut self, copy: usize, data: &[u8]) -> Result<(), Error> {
            if let Some(left) = self.writes_left.as_mut() {
                if *left == 0 {
                    // Half of the new data reached the disk.
                    self.copies[copy] = Some(data[..data.len() / 2].to_vec());
                    return Err(Error::Storage("power cut".into()));
                }
                *left -= 1;
            }
            self.copies[copy] = Some(data.to_vec());
            Ok(())
        }
    }

    fn installed() -> Disk {
        let mut disk = Disk::default();
        disk.copies[1] = Some(Selector::install("2026.11.0").unwrap().encode());
        disk
    }

    /// The whole update of slot B, through its confirmation.
    fn update(disk: &mut Disk) -> Result<(), Error> {
        let slot = begin_update(disk)?;
        finish_update(disk, slot, "2026.12.0")?;
        request_trial(disk, slot)?;
        assert_eq!(choose_boot(disk)?, Boot::Slot(slot));
        confirm(disk, slot)
    }

    #[test]
    fn encodes_and_rejects_damaged_selectors() {
        let selector = Selector::install("2026.11.0").unwrap();
        assert_eq!(Selector::decode(&selector.encode()), Some(selector.clone()));
        let mut damaged = selector.encode();
        damaged[20] ^= 1;
        assert_eq!(Selector::decode(&damaged), None);
        assert!(Selector::install("../../etc").is_err());
    }

    #[test]
    fn confirms_an_update_and_falls_back_after_failed_trials() {
        let mut disk = installed();
        update(&mut disk).unwrap();
        assert_eq!(load(&disk).unwrap().confirmed, Some(Slot::B));
        assert_eq!(choose_boot(&mut disk), Ok(Boot::Slot(Slot::B)));

        // The next release never confirms: three attempts, then slot B again.
        let slot = begin_update(&mut disk).unwrap();
        assert_eq!(slot, Slot::A);
        finish_update(&mut disk, slot, "2027.01.0").unwrap();
        request_trial(&mut disk, slot).unwrap();
        for _ in 0..TRIAL_BOOTS {
            assert_eq!(choose_boot(&mut disk), Ok(Boot::Slot(Slot::A)));
        }
        assert_eq!(choose_boot(&mut disk), Ok(Boot::Slot(Slot::B)));
        assert_eq!(load(&disk).unwrap().slot(Slot::A).state, State::Bad);
        assert!(confirm(&mut disk, Slot::A).is_err());
    }

    #[test]
    fn refuses_steps_out_of_order() {
        let mut disk = installed();
        assert!(request_trial(&mut disk, Slot::B).is_err());
        assert!(finish_update(&mut disk, Slot::A, "2026.12.0").is_err());
        assert!(confirm(&mut disk, Slot::B).is_err());
        let slot = begin_update(&mut disk).unwrap();
        finish_update(&mut disk, slot, "2026.12.0").unwrap();
        request_trial(&mut disk, slot).unwrap();
        assert!(begin_update(&mut disk).is_err());
    }

    #[test]
    fn survives_a_power_cut_at_every_write() {
        // Count the writes of a whole update, then cut the power at each one.
        let mut counting = installed();
        counting.writes_left = Some(usize::MAX);
        update(&mut counting).unwrap();
        let writes = usize::MAX - counting.writes_left.unwrap();
        for cut in 0..writes {
            let mut disk = installed();
            disk.writes_left = Some(cut);
            assert!(update(&mut disk).is_err(), "cut at write {cut}");
            disk.writes_left = None;
            // After the power comes back, the computer boots a release that
            // was good or is on trial, never a torn or unverified slot.
            let selector = load(&disk).expect("a selector copy survives");
            match choose_boot(&mut disk).unwrap() {
                Boot::Slot(slot) => {
                    let state = load(&disk).unwrap().slot(slot).state.clone();
                    assert!(matches!(state, State::Good | State::Trial(_)), "cut {cut}: booted {state:?}");
                }
                Boot::Recovery => panic!("cut {cut}: recovery with {selector:?}"),
            }
        }
    }

    #[test]
    fn boots_recovery_without_a_valid_selector() {
        let mut disk = Disk::default();
        assert_eq!(choose_boot(&mut disk), Ok(Boot::Recovery));
        disk.copies = [Some(b"garbage".to_vec()), Some(Vec::new())];
        assert_eq!(choose_boot(&mut disk), Ok(Boot::Recovery));
        assert_eq!(begin_update(&mut disk), Err(Error::NoSelector));
    }

    #[test]
    fn parses_and_checks_manifests() {
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let text = format!("release=2026.12.0\narch=x86_64\nschema=2..3\nfile=kernel 0 {digest}\nfile=initramfs 0 {digest}\n");
        let manifest = Manifest::parse(&text).unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(manifest.check_compatible("x86_64", 3).is_ok());
        assert!(manifest.check_compatible("x86_64", 1).is_err());
        assert!(manifest.check_compatible("aarch64", 2).is_err());
        for bad in [
            text.replace("file=kernel", "file=../kernel"),
            text.replace("schema=2..3", "schema=3..2"),
            text.replace("release=2026.12.0", "release=a b"),
            format!("{text}file=kernel 0 {digest}\n"),
            text.replace(digest, "00"),
        ] {
            assert!(Manifest::parse(&bad).is_err(), "{bad}");
        }
    }
}
