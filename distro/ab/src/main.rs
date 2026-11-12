//! `openshell-ab`: the A/B transaction over a directory that stands for the
//! system disk: `selector.0`, `selector.1`, `slot-a/` and `slot-b/`.

use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use openshell_ab::sha256::{hex, Sha256};
use openshell_ab::{
    begin_update, choose_boot, confirm, finish_update, load, request_trial, Boot, Error, Manifest, Selector, Slot,
    State, Store,
};

const USAGE: &str = "Usage: openshell-ab <disk-dir> <command>
  install <release>                      first release in slot a, confirmed
  status                                 the selector
  stage <source-dir> <manifest> <schema> write, verify and stage the other slot
  trial                                  boot the staged slot on trial
  boot                                   choose the slot to boot: a, b or recovery
  confirm <a|b>                          confirm the running slot";

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

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
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
        ("stage", [source, manifest, schema]) => {
            let manifest = Manifest::parse(&fs::read_to_string(manifest)?)?;
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
        ("confirm", [slot]) => confirm(&mut store, Slot::parse(slot).ok_or("slot is a or b")?)?,
        _ => return Err(USAGE.into()),
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
