//! `openshell-ab`: the A/B transaction over a directory that stands for the
//! system disk: `selector.0`, `selector.1`, `slot-a/` and `slot-b/`, and
//! `grubenv`, the summary GRUB boots from (src/bootenv.rs). On the disk image
//! (distro/image) the directory is on the data partition, and the slots are
//! the system partitions.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use openshell_ab::bootenv::{BootEnv, BLOCK_SIZE};
use openshell_ab::sha256::{hex, Sha256};
use openshell_ab::signature;
use openshell_ab::{
    begin_update, choose_boot, confirm, finish_update, load, record_boot, request_trial, restore_previous, Boot, Error,
    Manifest, Selector, Slot, State, Store,
};

const USAGE: &str = "Usage: openshell-ab <disk-dir> <command>
  install <release>                      first release in slot a, confirmed
  status                                 the selector
  stage <source-dir> <manifest> <schema> <keys-dir>
                                         check <manifest>.sig against the keys,
                                         then write, verify and stage the other slot
  update-mounted <source-dir> <manifest> <schema> <keys-dir> <slot-mount>
                                         update the mounted inactive partition and schedule a trial
  update-offline <source-dir> <manifest> <schema> <keys-dir> <slot-mount>
                                         the same from another system (the USB stick's updater):
                                         the partition of the slot that is not confirmed
  restore                                the previous release (the slot not confirmed) on trial
  trial                                  boot the staged slot on trial
  boot                                   choose the slot to boot: a, b or recovery
  booted <a|b|recovery>                  record the boot GRUB chose (its grubenv)
  confirm <a|b> [release]                confirm the running slot and, if given, verify its release
Every change rewrites <disk-dir>/grubenv, in place, for GRUB.
The release tooling:
  openshell-ab keygen <private.pem> <public.pem>
  openshell-ab sign <private.pem> <manifest>   writes <manifest>.sig";

struct DirectoryStore(PathBuf);

impl Store for DirectoryStore {
    fn read(&self, copy: usize) -> Option<Vec<u8>> {
        fs::read(self.0.join(format!("selector.{copy}"))).ok()
    }

    fn write(&mut self, copy: usize, data: &[u8]) -> Result<(), Error> {
        let storage = |error: std::io::Error| Error::Storage(error.to_string());
        let path = self.0.join(format!("selector.{copy}"));
        let mut file = File::create(&path).map_err(storage)?;
        file.write_all(data).map_err(storage)?;
        file.sync_all().map_err(storage)?;
        File::open(&self.0).and_then(|directory| directory.sync_all()).map_err(storage)
    }
}

/// Rewrites GRUB's environment block in place: GRUB's save_env writes into
/// the blocks the file already has, so the file is never truncated or moved.
fn write_boot_env(disk: &Path, selector: Option<&Selector>) -> std::io::Result<()> {
    let path = disk.join("grubenv");
    let created = !path.exists();
    let mut file = OpenOptions::new().write(true).create(true).truncate(false).open(&path)?;
    file.write_all(&BootEnv::from_selector(selector).encode())?;
    file.sync_all()?;
    if created {
        File::open(disk)?.sync_all()?;
    }
    Ok(())
}

/// What GRUB left in the environment: without a readable block, the summary
/// GRUB was given, as though it could not record the boot.
fn read_boot_env(disk: &Path, selector: Option<&Selector>) -> BootEnv {
    fs::read(disk.join("grubenv"))
        .ok()
        .filter(|block| block.len() == BLOCK_SIZE)
        .and_then(|block| BootEnv::decode(&block))
        .unwrap_or_else(|| BootEnv::from_selector(selector))
}

fn digest(path: &Path) -> std::io::Result<(u64, [u8; 32])> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::default();
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok((size, hasher.finish()));
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
}

/// Copies the release into the slot, then verifies what reached the disk.
fn write_slot(source: &Path, slot_dir: &Path, manifest: &Manifest) -> Result<(), Box<dyn std::error::Error>> {
    if slot_dir.exists() {
        fs::remove_dir_all(slot_dir)?;
    }
    fs::create_dir_all(slot_dir)?;
    write_release(source, slot_dir, manifest)
}

fn write_release(source: &Path, slot_dir: &Path, manifest: &Manifest) -> Result<(), Box<dyn std::error::Error>> {
    for (name, _, _) in &manifest.files {
        let target = slot_dir.join(name);
        let mut input = File::open(source.join(name))?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(&target)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
    }
    File::open(slot_dir)?.sync_all()?;
    for (name, size, expected) in &manifest.files {
        let (actual_size, actual) = digest(&slot_dir.join(name))?;
        if actual_size != *size || actual != *expected {
            return Err(format!("{name} does not match the manifest (sha256 {})", hex(&actual)).into());
        }
    }
    Ok(())
}

/// Returns the verified manifest before any change to the disk.
fn signed_manifest(path: &str, schema: &str, keys: &str) -> Result<Manifest, Box<dyn std::error::Error>> {
    let text = fs::read(path)?;
    let signed = fs::read_to_string(format!("{path}.sig")).map_err(|_| Error::Manifest("no signature".into()))?;
    if !signature::verify(&text, &signed, &trusted_keys(Path::new(keys))?) {
        return Err(Error::Manifest("not signed by a trusted key".into()).into());
    }
    let manifest = Manifest::parse(std::str::from_utf8(&text)?)?;
    manifest.check_compatible(env::consts::ARCH, schema.parse()?)?;
    Ok(manifest)
}

/// The slot the running system booted, from its command line.
fn booted_slot() -> Result<(Option<Slot>, Option<String>), Box<dyn std::error::Error>> {
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    let value = |key| cmdline.split_whitespace().find_map(|word| word.strip_prefix(key)).map(str::to_owned);
    Ok((value("openshell.slot=").as_deref().and_then(Slot::parse), value("openshell.release=")))
}

/// This updater uses the GPT layout made by distro/image/mkimage.sh. Derive
/// the expected partition from the mounted data disk and the running slot.
fn mounted_target(disk: &Path, target: &Path, selector: &Selector) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let (running, release) = booted_slot()?;
    let running = running.ok_or("not booted from a system slot")?;
    let current = selector.slot(running);
    if selector.confirmed != Some(running) || current.state != State::Good || release.as_deref() != Some(current.release.as_str()) {
        return Err("the running release must be the confirmed slot".into());
    }
    partition_target(disk, target, running)
}

/// The same from another system (the USB stick's updater): the confirmed
/// slot is the one that stays, its other one is written.
fn offline_target(disk: &Path, target: &Path, selector: &Selector) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if booted_slot()?.0.is_some() {
        return Err("an installed system updates itself with update-mounted".into());
    }
    let confirmed = selector.confirmed.ok_or("the installation has no confirmed slot")?;
    if selector.slot(confirmed).state != State::Good {
        return Err("the installation's confirmed slot is not good".into());
    }
    partition_target(disk, target, confirmed)
}

/// `target` must be the inactive system partition (the other one than
/// `running`'s) of the disk whose partition 5 is the mounted data.
fn partition_target(disk: &Path, target: &Path, running: Slot) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let disk = disk.canonicalize()?;
    let target = target.canonicalize()?;
    let data = disk.parent().and_then(Path::parent).ok_or("expected <data>/openshell/ab")?;
    if disk != data.join("openshell/ab") {
        return Err("expected <data>/openshell/ab".into());
    }
    let mounts = fs::read_to_string("/proc/mounts")?;
    let writable_device = |path: &Path| {
        mounts.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = fields.next()?;
            let mount = fields.next()?;
            let fs_type = fields.next()?;
            let flags = fields.next()?;
            (Path::new(mount) == path).then_some((device, fs_type, flags))
        }).last()
            .filter(|(_, fs_type, flags)| *fs_type == "ext2" && flags.split(',').any(|flag| flag == "rw"))
            .map(|(device, _, _)| device)
    };
    let drive = writable_device(data).and_then(|device| device.strip_prefix("/dev/"))
        .and_then(|device| device.strip_suffix('5'))
        .filter(|stem| {
            match stem.strip_suffix('p') {
                Some(name) if name.ends_with(|c: char| c.is_ascii_digit()) => {
                    name.bytes().any(|byte| byte.is_ascii_alphabetic())
                }
                _ => stem.ends_with(|c: char| c.is_ascii_alphabetic()),
            }
        })
        .filter(|stem| stem.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .ok_or("data must be mounted from partition 5 of the OpenShell disk")?;
    let partition = if running == Slot::A { 3 } else { 2 };
    if writable_device(&target) != Some(format!("/dev/{drive}{partition}").as_str()) {
        return Err("target is not the mounted inactive system partition".into());
    }
    Ok(target)
}

fn update_mounted(
    store: &mut DirectoryStore, source: &Path, target: &Path, manifest: &Manifest, offline: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    for required in ["kernel", "initramfs.cpio.gz", "boot.cfg"] {
        if !manifest.files.iter().any(|(name, size, _)| name == required && *size > 0) {
            return Err(Error::Manifest(format!("missing boot artifact {required}")).into());
        }
    }
    let selector = load(store).ok_or(Error::NoSelector)?;
    let target = if offline { offline_target(&store.0, target, &selector)? } else { mounted_target(&store.0, target, &selector)? };
    let source = source.canonicalize()?;
    if source.starts_with(&target) {
        return Err("source must be outside the inactive partition".into());
    }
    let slot = begin_update(store)?;
    write_boot_env(&store.0, load(store).as_ref())?;
    for entry in fs::read_dir(&target)? {
        let entry = entry?;
        if entry.file_name() == "lost+found" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            return Err("unexpected directory in the inactive system partition".into());
        }
        fs::remove_file(entry.path())?;
    }
    write_release(&source, &target, manifest)?;
    finish_update(store, slot, &manifest.release)?;
    request_trial(store, slot)?;
    println!("scheduled {} for trial in slot {}", manifest.release, slot.name());
    Ok(())
}

/// The trusted keys: every *.pem in the folder.
fn trusted_keys(folder: &Path) -> Result<Vec<ed25519_dalek::VerifyingKey>, Box<dyn std::error::Error>> {
    let mut keys = Vec::new();
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "pem") {
            keys.push(signature::public_key(&fs::read_to_string(&path)?).ok_or_else(|| format!("{} is not an Ed25519 public key", path.display()))?);
        }
    }
    if keys.is_empty() {
        return Err(format!("no trusted keys in {}", folder.display()).into());
    }
    Ok(keys)
}

fn tooling(args: &[String]) -> Option<Result<(), Box<dyn std::error::Error>>> {
    match args {
        [command, private, public] if command == "keygen" => Some((|| {
            let mut seed = [0u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut seed)?;
            let key = ed25519_dalek::SigningKey::from_bytes(&seed);
            fs::write(private, signature::private_pem(&key))?;
            fs::write(public, signature::public_pem(&key.verifying_key()))?;
            Ok(())
        })()),
        [command, private, manifest] if command == "sign" => Some((|| {
            let key = signature::private_key(&fs::read_to_string(private)?).ok_or("not an Ed25519 private key")?;
            fs::write(format!("{manifest}.sig"), signature::sign(&fs::read(manifest)?, &key))?;
            Ok(())
        })()),
        _ => None,
    }
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(result) = tooling(args) {
        return result;
    }
    let [disk, command, rest @ ..] = args else { return Err(USAGE.into()) };
    let disk = PathBuf::from(disk);
    if command == "install" {
        fs::create_dir_all(&disk)?;
    }
    // Use an inode that stays in place for every writer of the selector and grubenv.
    let _lock = if command == "status" {
        None
    } else {
        let lock = File::open(&disk)?;
        lock.lock()?;
        Some(lock)
    };
    let mut store = DirectoryStore(disk.clone());
    match (command.as_str(), rest) {
        ("install", [release]) => {
            if load(&store).is_some() {
                return Err("the disk already has a selector".into());
            }
            store.write(1, &Selector::install(release)?.encode())?;
        }
        ("status", []) => {
            let selector = load(&store).ok_or(Error::NoSelector)?;
            for slot in [Slot::A, Slot::B] {
                let record = selector.slot(slot);
                let mark = if selector.confirmed == Some(slot) { " (confirmed)" } else { "" };
                println!("{}: {:?} {}{mark}", slot.name(), record.state, record.release);
            }
            println!("generation {}", selector.generation);
        }
        ("stage", [source, manifest, schema, keys]) => {
            let manifest = signed_manifest(manifest, schema, keys)?;
            let slot = begin_update(&mut store)?;
            write_slot(Path::new(source), &disk.join(format!("slot-{}", slot.name())), &manifest)?;
            finish_update(&mut store, slot, &manifest.release)?;
            println!("staged {} in slot {}", manifest.release, slot.name());
        }
        ("update-mounted", [source, manifest, schema, keys, target]) => {
            let manifest = signed_manifest(manifest, schema, keys)?;
            update_mounted(&mut store, Path::new(source), Path::new(target), &manifest, false)?;
        }
        ("update-offline", [source, manifest, schema, keys, target]) => {
            let manifest = signed_manifest(manifest, schema, keys)?;
            update_mounted(&mut store, Path::new(source), Path::new(target), &manifest, true)?;
        }
        ("restore", []) => {
            let slot = restore_previous(&mut store)?;
            let selector = load(&store).ok_or(Error::NoSelector)?;
            println!("restoring {} in slot {}", selector.slot(slot).release, slot.name());
        }
        ("trial", []) => {
            let selector = load(&store).ok_or(Error::NoSelector)?;
            let slot = [Slot::A, Slot::B]
                .into_iter()
                .find(|slot| selector.slot(*slot).state == State::Staged)
                .ok_or("no staged release")?;
            request_trial(&mut store, slot)?;
        }
        ("boot", []) => match choose_boot(&mut store)? {
            Boot::Slot(slot) => println!("{}", slot.name()),
            Boot::Recovery => println!("recovery"),
        },
        ("booted", [slot]) => {
            let booted = match slot.as_str() {
                "recovery" => Boot::Recovery,
                name => Boot::Slot(Slot::parse(name).ok_or("slot is a, b or recovery")?),
            };
            let env = read_boot_env(&disk, load(&store).as_ref());
            record_boot(&mut store, booted, &env)?;
        }
        ("confirm", [slot, release @ ..]) if release.len() <= 1 => {
            let slot = Slot::parse(slot).ok_or("slot is a or b")?;
            if let [release] = release {
                let selector = load(&store).ok_or(Error::NoSelector)?;
                if selector.slot(slot).release != *release {
                    return Err("running release does not match the slot".into());
                }
            }
            confirm(&mut store, slot)?;
        }
        _ => return Err(USAGE.into()),
    }
    if command != "status" {
        write_boot_env(&disk, load(&store).as_ref())?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openshell-ab: {error}");
            ExitCode::FAILURE
        }
    }
}
