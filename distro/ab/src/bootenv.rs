//! The boot environment GRUB reads (distro/image/grub.cfg): a GRUB
//! environment block, 1024 bytes that GRUB rewrites in place with `save_env`.
//!
//! GRUB cannot read the selector (two copies with a checksum), so the system
//! writes it a summary after every change of the selector: the confirmed slot
//! to fall back to and the slot on trial, with its attempts left as a run of
//! `1`s. GRUB has no arithmetic: it takes a `1` off with `regexp` and saves
//! the rest before it boots a trial. The booted system reads back what GRUB
//! recorded (`record_boot`), so the selector stays the one record of the
//! transaction, and the two ways to boot choose alike (see the tests).

use crate::{Boot, Selector, Slot, State};

pub const BLOCK_SIZE: usize = 1024;
const HEADER: &str = "# GRUB Environment Block\n";
pub const FALLBACK: &str = "openshell_fallback";
pub const TRIAL: &str = "openshell_trial";
pub const TRIES: &str = "openshell_tries";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootEnv {
    /// The confirmed slot, when it is good; otherwise GRUB boots the recovery.
    pub fallback: Option<Slot>,
    /// The slot on trial and the attempts it has left.
    pub trial: Option<(Slot, u8)>,
}

impl BootEnv {
    pub fn from_selector(selector: Option<&Selector>) -> Self {
        let Some(selector) = selector else { return Self { fallback: None, trial: None } };
        let fallback = selector.confirmed.filter(|slot| selector.slot(*slot).state == State::Good);
        let trial = [Slot::A, Slot::B].into_iter().find_map(|slot| match selector.slot(slot).state {
            State::Trial(tries) => Some((slot, tries)),
            _ => None,
        });
        Self { fallback, trial }
    }

    pub fn encode(&self) -> [u8; BLOCK_SIZE] {
        let mut text = String::from(HEADER);
        text += &format!("{FALLBACK}={}\n", self.fallback.map_or("recovery", Slot::name));
        if let Some((slot, tries)) = self.trial {
            text += &format!("{TRIAL}={}\n{TRIES}={}\n", slot.name(), "1".repeat(tries.into()));
        }
        let mut block = [b'#'; BLOCK_SIZE];
        block[..text.len()].copy_from_slice(text.as_bytes());
        block
    }

    /// Reads a block as GRUB left it. Unknown variables are ignored.
    pub fn decode(block: &[u8]) -> Option<Self> {
        if block.len() != BLOCK_SIZE {
            return None;
        }
        let text = std::str::from_utf8(block).ok()?.strip_prefix(HEADER)?;
        let (mut fallback, mut trial, mut tries) = (None, None, 0u8);
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let Some((name, value)) = line.split_once('=') else { continue };
            match name {
                FALLBACK => fallback = Some(if value == "recovery" { None } else { Some(Slot::parse(value)?) }),
                TRIAL => trial = Some(Slot::parse(value)?),
                TRIES if value.bytes().all(|byte| byte == b'1') => tries = u8::try_from(value.len()).ok()?,
                TRIES => return None,
                _ => {}
            }
        }
        Some(Self { fallback: fallback?, trial: trial.map(|slot| (slot, tries)) })
    }

    /// What grub.cfg does: a trial with attempts left boots and loses one;
    /// otherwise the confirmed slot boots, or the recovery.
    pub fn grub_boot(&self) -> (Boot, BootEnv) {
        match self.trial {
            Some((slot, tries)) if tries > 0 => (Boot::Slot(slot), Self { trial: Some((slot, tries - 1)), ..self.clone() }),
            _ => (self.fallback.map_or(Boot::Recovery, Boot::Slot), self.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::staged;
    use crate::{choose_boot, confirm, load, record_boot, request_trial};

    #[test]
    fn a_block_is_what_grub_reads() {
        let env = BootEnv { fallback: Some(Slot::A), trial: Some((Slot::B, 3)) };
        let block = env.encode();
        let text = std::str::from_utf8(&block).unwrap();
        assert!(text.starts_with("# GRUB Environment Block\nopenshell_fallback=a\nopenshell_trial=b\nopenshell_tries=111\n#"));
        assert!(text.trim_end_matches('#').ends_with('\n'), "padded with #");
        assert_eq!(BootEnv::decode(&block), Some(env));
        let none = BootEnv::from_selector(None);
        assert_eq!(BootEnv::decode(&none.encode()), Some(none.clone()));
        assert_eq!(none.grub_boot().0, Boot::Recovery);
        assert_eq!(BootEnv::decode(&block[..512]), None);
        assert_eq!(BootEnv::decode(&[b'#'; BLOCK_SIZE]), None);
    }

    #[test]
    fn grub_counts_the_attempts_of_a_trial() {
        let mut env = BootEnv { fallback: Some(Slot::A), trial: Some((Slot::B, 3)) };
        let mut boots = Vec::new();
        for _ in 0..5 {
            let (boot, next) = env.grub_boot();
            boots.push(boot);
            env = next;
        }
        let (a, b) = (Boot::Slot(Slot::A), Boot::Slot(Slot::B));
        assert_eq!(boots, [b, b, b, a, a]);
    }

    /// Booting through GRUB and recording the boot chooses as the selector's own boot step does.
    #[test]
    fn grub_and_the_selector_choose_alike() {
        // Each plan: how many boots of the trial pass before the system confirms it (None: never).
        for confirm_after in [None, Some(1), Some(2), Some(3)] {
            let mut by_selector = staged();
            let mut by_grub = staged();
            for store in [&mut by_selector, &mut by_grub] {
                request_trial(store, Slot::B).unwrap();
            }
            let mut env = BootEnv::from_selector(load(&by_grub).as_ref());
            for boot_number in 1..=5 {
                let chosen = choose_boot(&mut by_selector).unwrap();
                let (grub, next) = env.grub_boot();
                record_boot(&mut by_grub, grub, &next).unwrap();
                assert_eq!(grub, chosen, "boot {boot_number} with confirmation after {confirm_after:?}");
                if confirm_after == Some(boot_number) {
                    if let Boot::Slot(slot) = chosen {
                        confirm(&mut by_selector, slot).unwrap();
                        confirm(&mut by_grub, slot).unwrap();
                    }
                }
                env = BootEnv::from_selector(load(&by_grub).as_ref());
                let (left, right) = (load(&by_selector).unwrap(), load(&by_grub).unwrap());
                assert_eq!((left.slots, left.confirmed), (right.slots, right.confirmed), "boot {boot_number}");
            }
        }
    }

    #[test]
    fn a_boot_grub_could_not_record_still_counts() {
        let mut store = staged();
        request_trial(&mut store, Slot::B).unwrap();
        // save_env failed: the environment still says three.
        let env = BootEnv::from_selector(load(&store).as_ref());
        record_boot(&mut store, Boot::Slot(Slot::B), &env).unwrap();
        assert_eq!(load(&store).unwrap().slot(Slot::B).state, State::Trial(2));
    }
}
