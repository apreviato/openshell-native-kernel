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
    begin_update, choose_boot, confirm, finish_update, load, record_boot, request_trial, Boot, Error, Manifest,
    Selector, Slot, State, Store,
};

const USAGE: &str = "Usage: openshell-ab <disk-dir> <command>
  install <release>                      first release in slot a, confirmed
  status                                 the selector
  stage <source-dir> <manifest> <schema> <keys-dir>
                                         check <manifest>.sig against the keys,
                                         then write, verify and stage the other slot
  trial                                  boot the staged slot on trial
  boot                                   choose the slot to boot: a, b or recovery
  booted <a|b|recovery>                  record the boot GRUB chose (its grubenv)
  confirm <a|b>                          confirm the running slot
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
    for (name, _, _) in &manifest.files {
        let target = slot_dir.join(name);
        fs::copy(source.join(name), &target)?;
        File::open(&target)?.sync_all()?;
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
    let mut store = DirectoryStore(disk.clone());
    match (command.as_str(), rest) {
        ("install", [release]) => {
            if load(&store).is_some() {
                return Err("the disk already has a selector".into());
            }
            fs::create_dir_all(&disk)?;
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
            // The signature first: nothing of an unsigned manifest is read.
            let text = fs::read(manifest)?;
            let signed = fs::read_to_string(format!("{manifest}.sig")).map_err(|_| Error::Manifest("no signature".into()))?;
            if !signature::verify(&text, &signed, &trusted_keys(Path::new(keys))?) {
                return Err(Error::Manifest("not signed by a trusted key".into()).into());
            }
            let manifest = Manifest::parse(std::str::from_utf8(&text)?)?;
            manifest.check_compatible(env::consts::ARCH, schema.parse()?)?;
            let slot = begin_update(&mut store)?;
            write_slot(Path::new(source), &disk.join(format!("slot-{}", slot.name())), &manifest)?;
            finish_update(&mut store, slot, &manifest.release)?;
            println!("staged {} in slot {}", manifest.release, slot.name());
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
        ("confirm", [slot]) => confirm(&mut store, Slot::parse(slot).ok_or("slot is a or b")?)?,
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
