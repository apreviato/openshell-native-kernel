//! Disks and removable media for the session's Drives page, as udisks2
//! gives them on OpenShell Live: what each block device holds, and mounting
//! it under `/media/<user>`, unmounting and ejecting it, which take root.
//!
//! drives                  → "drives <lsblk -J's JSON>"
//! drives mount <device>   → "mounted <path>" | "failed <why>" | "invalid"
//! drives unmount <device> → "ok" | "busy" | "failed <why>" | "invalid"
//! drives eject <disk>     → "ok" | "busy" | "failed <why>" | "invalid"

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DRIVES_REQUEST: &str = "drives";

/// How much of a device identifying it reads (ISO 9660 is at 32 KiB).
const HEAD_BYTES: usize = 36 * 1024;
const MEDIA: &str = "/media";
/// Mount flags: no set-user-ID programs, no device nodes (udisks2's).
const MS_NOSUID: u64 = 1 << 1;
const MS_NODEV: u64 = 1 << 2;
/// Where the system lives: the disks holding these are the system's.
const SYSTEM_MOUNTS: [&str; 6] = ["/", "/data", "/boot", "/boot/efi", "/nix", "/usr"];

/// What each device was found to hold, by its folder and name, with its
/// size then: the Desktop asks every few seconds, and a stick is not read
/// each time. (The kernel names each disk it attaches anew: sda, sdb, ...)
/// What each device held, by its folder and name: its size then, its volume.
type VolumeCache = BTreeMap<(PathBuf, String), (u64, Option<Volume>)>;
static VOLUMES: Mutex<VolumeCache> = Mutex::new(BTreeMap::new());

pub fn is_request(line: &str) -> bool {
    line == DRIVES_REQUEST || line.starts_with("drives ")
}

/// The block devices of `/proc/partitions`: names and sizes in bytes.
pub fn parse_partitions(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields[..] {
                [_, _, blocks, name] if blocks.parse::<u64>().is_ok() => Some((name.to_owned(), blocks.parse::<u64>().ok()? * 1024)),
                _ => None,
            }
        })
        .collect()
}

/// The disk a partition is on (`sda1` → `sda`, `nvme0n1p2` → `nvme0n1`).
pub fn parent_disk<'a>(name: &str, names: &[&'a str]) -> Option<&'a str> {
    names
        .iter()
        .filter(|disk| disk.len() < name.len() && name.starts_with(**disk))
        .filter(|disk| {
            let rest = &name[disk.len()..];
            let digits = rest.strip_prefix('p').filter(|_| disk.ends_with(|c: char| c.is_ascii_digit())).unwrap_or(rest);
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        })
        .max_by_key(|disk| disk.len())
        .copied()
}

/// `/proc/mounts`: (source, target, type), octal escapes undone.
pub fn parse_mounts(text: &str) -> Vec<(String, String, String)> {
    fn unescape(field: &str) -> String {
        let bytes = field.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\\' && index + 3 < bytes.len() && bytes[index + 1..index + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
                out.push((bytes[index + 1] - b'0') * 64 + (bytes[index + 2] - b'0') * 8 + (bytes[index + 3] - b'0'));
                index += 4;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            Some((unescape(fields.next()?), unescape(fields.next()?), fields.next()?.to_owned()))
        })
        .collect()
}

/// What a device holds: its filesystem type, label and UUID as blkid
/// names them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volume {
    pub fstype: String,
    pub label: String,
    pub uuid: String,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim_end().to_owned()
}

fn serial(value: u32) -> String {
    format!("{:04X}-{:04X}", value >> 16, value & 0xffff)
}

/// Identifies a device from its first bytes; `read_at` reads elsewhere
/// (exFAT keeps its label in the root directory).
pub fn identify(head: &[u8], read_at: &mut dyn FnMut(u64, &mut [u8]) -> bool) -> Option<Volume> {
    if head.len() < 4096 {
        return None;
    }
    // ext2, ext3 and ext4: the superblock at 1 KiB.
    if head.len() >= 2048 && le16(head, 1024 + 56) == 0xef53 {
        let sb = &head[1024..2048];
        let (compat, incompat) = (le32(sb, 92), le32(sb, 96));
        let fstype = if incompat & (0x40 | 0x80 | 0x200) != 0 {
            "ext4"
        } else if compat & 0x4 != 0 {
            "ext3"
        } else {
            "ext2"
        };
        let u = &sb[104..120];
        let uuid = format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7], u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15]
        );
        return Some(Volume { fstype: fstype.into(), label: text(&sb[120..136]), uuid });
    }
    if &head[3..11] == b"EXFAT   " {
        let label = exfat_label(head, read_at).unwrap_or_default();
        return Some(Volume { fstype: "exfat".into(), label, uuid: serial(le32(head, 100)) });
    }
    if &head[3..11] == b"NTFS    " {
        let serial = u64::from_le_bytes(head[72..80].try_into().ok()?);
        return Some(Volume { fstype: "ntfs".into(), label: String::new(), uuid: format!("{serial:016X}") });
    }
    // FAT: a BIOS parameter block, with "FAT" where its kind is written.
    let fat32 = &head[82..85] == b"FAT";
    if (fat32 || &head[54..57] == b"FAT") && le16(head, 11).is_power_of_two() && head[13] != 0 && le16(head, 510) == 0xaa55 {
        let (label_at, serial_at) = if fat32 { (71, 67) } else { (43, 39) };
        let label = text(&head[label_at..label_at + 11]);
        let label = if label == "NO NAME" { String::new() } else { label };
        return Some(Volume { fstype: "vfat".into(), label, uuid: serial(le32(head, serial_at)) });
    }
    if head.len() >= 32 * 1024 + 6 + 72 && &head[32 * 1024 + 1..32 * 1024 + 6] == b"CD001" {
        let pvd = &head[32 * 1024..];
        return Some(Volume { fstype: "iso9660".into(), label: text(&pvd[40..72]), uuid: String::new() });
    }
    if &head[4096 - 10..4096] == b"SWAPSPACE2" {
        return Some(Volume { fstype: "swap".into(), ..Default::default() });
    }
    None
}

/// exFAT's volume label: the 0x83 entry of its root directory.
fn exfat_label(boot: &[u8], read_at: &mut dyn FnMut(u64, &mut [u8]) -> bool) -> Option<String> {
    let sector_shift = u32::from(boot[108]);
    let cluster_shift = sector_shift + u32::from(boot[109]);
    if !(9..=12).contains(&sector_shift) || cluster_shift > 25 {
        return None;
    }
    let heap = u64::from(le32(boot, 88)) << sector_shift;
    let root = u64::from(le32(boot, 96));
    if root < 2 {
        return None;
    }
    let mut cluster = vec![0u8; (1usize << cluster_shift).min(64 * 1024)];
    if !read_at(heap + ((root - 2) << cluster_shift), &mut cluster) {
        return None;
    }
    let entry = cluster.as_chunks::<32>().0.iter().find(|entry| entry[0] == 0x83)?;
    let count = usize::from(entry[1]).min(11);
    let units: Vec<u16> = (0..count).map(|i| le16(entry, 2 + i * 2)).collect();
    Some(String::from_utf16_lossy(&units))
}

/// The program that serves NTFS volumes (through FUSE), as Debian mounts
/// them with `mount.ntfs-3g`.
pub const NTFS_SERVER: &str = "/opt/openshell-compat/bin/ntfs-3g";

/// The "filesystem type" of a volume a program serves: [`MountOps::mount`]
/// runs [`NTFS_SERVER`], which mounts it and serves it until it is unmounted.
pub const NTFS_TYPE: &str = "ntfs-3g";

/// The filesystem type the kernel mounts a volume as, with its options.
pub fn mount_type(volume: &Volume, uid: u32, gid: u32) -> Result<(&'static str, String), String> {
    match volume.fstype.as_str() {
        // udisks2's options: the files the user's, not shown to others.
        "vfat" => Ok(("vfat", format!("uid={uid},gid={gid},dmask=0077,fmask=0177,shortname=mixed,utf8,flush"))),
        "exfat" => Ok(("exfat", format!("uid={uid},gid={gid},dmask=0077,fmask=0177"))),
        // udisks2's for ntfs-3g: names Windows can read back.
        "ntfs" => Ok((NTFS_TYPE, format!("uid={uid},gid={gid},dmask=0077,fmask=0177,windows_names"))),
        "ext2" => Ok(("ext2", String::new())),
        "" => Err("the drive holds no file system that can be read".into()),
        other => Err(format!("{other} drives cannot be opened yet")),
    }
}

/// The folder a volume is mounted at: its label (or UUID, or device),
/// made safe, with a number added when that is taken.
pub fn mount_folder(user_media: &Path, volume: &Volume, device: &str, taken: &dyn Fn(&Path) -> bool) -> PathBuf {
    let base = [volume.label.as_str(), volume.uuid.as_str(), device.rsplit('/').next().unwrap_or("disk")]
        .into_iter()
        .map(|name| name.chars().map(|c| if c == '/' || c.is_control() { '_' } else { c }).collect::<String>())
        .map(|name| name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "disk".into());
    let mut folder = user_media.join(&base);
    let mut number = 1;
    while taken(&folder) {
        folder = user_media.join(format!("{base}{number}"));
        number += 1;
    }
    folder
}

fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A device as lsblk's JSON gives it.
pub struct Device {
    pub name: String,
    pub size: u64,
    pub volume: Option<Volume>,
    pub mounts: Vec<String>,
    pub removable: bool,
    pub transport: &'static str,
    pub children: Vec<Device>,
}

fn device_json(out: &mut String, device: &Device, kind: &str) {
    out.push_str("{\"name\":");
    json_string(out, &device.name);
    out.push_str(",\"path\":");
    json_string(out, &format!("/dev/{}", device.name));
    let _ = write!(out, ",\"size\":{},\"type\":\"{kind}\"", device.size);
    let volume = device.volume.clone().unwrap_or_default();
    for (key, value) in [("fstype", &volume.fstype), ("label", &volume.label), ("uuid", &volume.uuid)] {
        let _ = write!(out, ",\"{key}\":");
        if value.is_empty() { out.push_str("null") } else { json_string(out, value) }
    }
    out.push_str(",\"mountpoints\":[");
    for (index, mount) in device.mounts.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        json_string(out, mount);
    }
    let _ = write!(
        out,
        "],\"rm\":{0},\"hotplug\":{0},\"model\":null,\"tran\":\"{1}\",\"ro\":false",
        device.removable, device.transport
    );
    if !device.children.is_empty() {
        out.push_str(",\"children\":[");
        for (index, child) in device.children.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            device_json(out, child, "part");
        }
        out.push(']');
    }
    out.push('}');
}

/// `lsblk -J`'s shape, which the Desktop reads on the Live system too.
pub fn lsblk_json(disks: &[Device]) -> String {
    let mut out = String::from("{\"blockdevices\":[");
    for (index, disk) in disks.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        device_json(&mut out, disk, "disk");
    }
    out.push_str("]}");
    out
}

/// Which disks there are, as lsblk names their transport: SATA disks (`sd*`
/// whose vendor is "ATA", as Linux's libata names it, in
/// /sys/block/<disk>/device/vendor), NVMe and VirtIO ones built in; USB disks
/// (the other `sd*`) and memory cards removable.
fn transport(sys_block: &Path, name: &str) -> (&'static str, bool) {
    if name.starts_with("sd") {
        let vendor = fs::read_to_string(sys_block.join(name).join("device/vendor")).unwrap_or_default();
        if vendor.trim() == "ATA" { ("sata", false) } else { ("usb", true) }
    } else if name.starts_with("nvme") {
        ("nvme", false)
    } else if name.starts_with("mmcblk") {
        ("mmc", true)
    } else {
        ("virtio", false)
    }
}

/// A device name the requests may carry.
pub fn valid_device(device: &str) -> Option<&str> {
    let name = device.strip_prefix("/dev/")?;
    (!name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())).then_some(name)
}

/// Mounting and unmounting, which the supervisor does with the
/// capability it raises only then.
pub trait MountOps {
    fn mount(&mut self, source: &str, target: &Path, fstype: &str, flags: u64, data: &str) -> io::Result<()>;
    fn unmount(&mut self, target: &Path) -> io::Result<()>;
}

/// The system's files the requests read: the block devices, the mounts.
pub struct SystemView {
    pub dev: PathBuf,
    /// /sys/block: what each disk says of itself.
    pub sys_block: PathBuf,
    pub partitions: PathBuf,
    pub mounts: PathBuf,
    pub media: PathBuf,
}

impl Default for SystemView {
    fn default() -> Self {
        Self { dev: "/dev".into(), sys_block: "/sys/block".into(), partitions: "/proc/partitions".into(), mounts: "/proc/mounts".into(), media: MEDIA.into() }
    }
}

impl SystemView {
    /// What a device holds, from the cache while its size is the same.
    fn volume(&self, name: &str, size: u64) -> Option<Volume> {
        let key = (self.dev.clone(), name.to_owned());
        if let Some((cached_size, volume)) = VOLUMES.lock().ok()?.get(&key) {
            if *cached_size == size {
                return volume.clone();
            }
        }
        let volume = self.read_volume(name);
        if let Ok(mut cache) = VOLUMES.lock() {
            cache.insert(key, (size, volume.clone()));
        }
        volume
    }

    /// Forgets what a disk and its partitions hold (mounted, ejected:
    /// looked at afresh).
    fn forget(&self, disk: &str) {
        if let Ok(mut cache) = VOLUMES.lock() {
            cache.retain(|(dev, name), _| !(dev == &self.dev && name.starts_with(disk)));
        }
    }

    fn read_volume(&self, name: &str) -> Option<Volume> {
        let mut file = fs::File::open(self.dev.join(name)).ok()?;
        let mut head = vec![0u8; HEAD_BYTES];
        let mut filled = 0;
        while filled < head.len() {
            match file.read(&mut head[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(_) => return None,
            }
        }
        head.truncate(filled);
        identify(&head, &mut |offset, buf| file.seek(SeekFrom::Start(offset)).is_ok() && file.read_exact(buf).is_ok())
    }

    fn mounts(&self) -> Vec<(String, String, String)> {
        parse_mounts(&fs::read_to_string(&self.mounts).unwrap_or_default())
    }

    /// The disks with their partitions; a disk without partitions that
    /// holds a filesystem (a "superfloppy" stick) is its own volume.
    pub fn devices(&self) -> Vec<Device> {
        let entries = parse_partitions(&fs::read_to_string(&self.partitions).unwrap_or_default());
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        let mounts = self.mounts();
        let mounted_at = |name: &str| -> Vec<String> {
            let device = format!("/dev/{name}");
            mounts.iter().filter(|(source, _, _)| *source == device).map(|(_, target, _)| target.clone()).collect()
        };
        let mut disks: Vec<Device> = Vec::new();
        for (name, size) in &entries {
            if name.starts_with("loop") || name.starts_with("ram") || parent_disk(name, &names).is_some() {
                continue;
            }
            let (transport, removable) = transport(&self.sys_block, name);
            let mut disk = Device {
                name: name.clone(),
                size: *size,
                volume: None,
                mounts: mounted_at(name),
                removable,
                transport,
                children: Vec::new(),
            };
            for (child, child_size) in entries.iter().filter(|(child, _)| parent_disk(child, &names) == Some(name.as_str())) {
                disk.children.push(Device {
                    name: child.clone(),
                    size: *child_size,
                    volume: self.volume(child, *child_size),
                    mounts: mounted_at(child),
                    removable,
                    transport,
                    children: Vec::new(),
                });
            }
            if disk.children.is_empty() {
                disk.volume = self.volume(name, *size);
            }
            disks.push(disk);
        }
        disks
    }

    fn is_system_disk(&self, disk: &Device) -> bool {
        let user_media = |mount: &str| mount.starts_with(&format!("{}/", self.media.display()));
        std::iter::once(disk).chain(disk.children.iter()).flat_map(|device| device.mounts.iter()).any(|mount| {
            !user_media(mount) && (SYSTEM_MOUNTS.contains(&mount.as_str()) || mount.starts_with("/data/"))
        })
    }

    fn find(&self, name: &str) -> Option<(Device, Option<Device>)> {
        for disk in self.devices() {
            if disk.name == name {
                return Some((disk, None));
            }
            if let Some(part) = disk.children.iter().position(|child| child.name == name) {
                let mut disk = disk;
                let part = disk.children.remove(part);
                return Some((disk, Some(part)));
            }
        }
        None
    }

    fn mount(&self, name: &str, user: &str, uid: u32, gid: u32, ops: &mut dyn MountOps) -> String {
        let Some((disk, part)) = self.find(name) else { return "failed the drive is not attached".into() };
        // A whole disk only when it has no partitions (a superfloppy).
        let device = match part {
            Some(part) => part,
            None if disk.children.is_empty() => disk,
            None => return "failed open one of the drive's partitions".into(),
        };
        if let Some(mount) = device.mounts.first() {
            return format!("mounted {mount}");
        }
        let Some(volume) = device.volume.clone() else { return "failed the drive holds no file system that can be read".into() };
        let (fstype, options) = match mount_type(&volume, uid, gid) {
            Ok(found) => found,
            Err(why) => return format!("failed {why}"),
        };
        let user_media = self.media.join(user);
        for folder in [&self.media, &user_media] {
            if !folder.is_dir() {
                if let Err(error) = fs::create_dir(folder) {
                    return format!("failed {}: {error}", folder.display());
                }
            }
        }
        let mounted: Vec<String> = self.mounts().into_iter().map(|(_, target, _)| target).collect();
        let folder = mount_folder(&user_media, &volume, name, &|path| {
            mounted.iter().any(|target| Path::new(target) == path) || fs::read_dir(path).map(|mut entries| entries.next().is_some()).unwrap_or(false)
        });
        if !folder.is_dir() {
            if let Err(error) = fs::create_dir(&folder) {
                return format!("failed {}: {error}", folder.display());
            }
        }
        match ops.mount(&format!("/dev/{name}"), &folder, fstype, MS_NOSUID | MS_NODEV, &options) {
            Ok(()) => format!("mounted {}", folder.display()),
            Err(error) => {
                let _ = fs::remove_dir(&folder);
                format!("failed {error}")
            }
        }
    }

    fn unmount_device(&self, device: &Device, ops: &mut dyn MountOps) -> Result<(), String> {
        for mount in &device.mounts {
            match ops.unmount(Path::new(mount)) {
                Ok(()) => {
                    if Path::new(mount).starts_with(&self.media) {
                        let _ = fs::remove_dir(mount);
                    }
                }
                Err(error) if error.raw_os_error() == Some(16) => return Err("busy".into()),
                Err(error) => return Err(format!("failed {error}")),
            }
        }
        Ok(())
    }

    pub fn answer(&self, line: &str, user: &str, uid: u32, gid: u32, ops: &mut dyn MountOps) -> String {
        let words: Vec<&str> = line.split(' ').collect();
        match words[..] {
            [DRIVES_REQUEST] => format!("drives {}", lsblk_json(&self.devices())),
            [DRIVES_REQUEST, "mount", device] => match valid_device(device) {
                Some(name) => {
                    self.forget(name);
                    self.mount(name, user, uid, gid, ops)
                }
                None => "invalid".into(),
            },
            [DRIVES_REQUEST, verb @ ("unmount" | "eject"), device] => {
                let Some(name) = valid_device(device) else { return "invalid".into() };
                let Some((disk, part)) = self.find(name) else { return "failed the drive is not attached".into() };
                self.forget(&disk.name);
                if self.is_system_disk(&disk) && (verb == "eject" || part.as_ref().is_none_or(|part| {
                    part.mounts.iter().any(|mount| SYSTEM_MOUNTS.contains(&mount.as_str()) || mount.starts_with("/data"))
                })) {
                    return "failed the system runs from that drive".into();
                }
                let devices: Vec<&Device> = match (&part, verb) {
                    (Some(part), "unmount") => vec![part],
                    _ => std::iter::once(&disk).chain(disk.children.iter()).collect(),
                };
                for device in devices {
                    if let Err(why) = self.unmount_device(device, ops) {
                        return why;
                    }
                }
                "ok".into()
            }
            _ => "invalid".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn reads_proc_partitions() {
        let text = "major minor  #blocks  name\n\n   8       0     102400 sda\n   8       1      32768 sda1\n 259 0 500107608 nvme0n1\n 259 1 524288 nvme0n1p1\n";
        let parts = parse_partitions(text);
        assert_eq!(parts[0], ("sda".into(), 102400 * 1024));
        let names: Vec<&str> = parts.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(parent_disk("sda1", &names), Some("sda"));
        assert_eq!(parent_disk("nvme0n1p1", &names), Some("nvme0n1"));
        assert_eq!(parent_disk("nvme0n1", &names), None);
        assert_eq!(parent_disk("sda", &names), None);
    }

    #[test]
    fn reads_mounts() {
        let mounts = parse_mounts("/dev/sda1 /media/ana/My\\040Stick vfat rw 0 0\nproc /proc proc rw 0 0\n");
        assert_eq!(mounts[0], ("/dev/sda1".into(), "/media/ana/My Stick".into(), "vfat".into()));
    }

    fn fat32_boot(label: &[u8; 11]) -> Vec<u8> {
        let mut b = vec![0u8; HEAD_BYTES];
        b[0] = 0xeb;
        b[3..11].copy_from_slice(b"mkfs.fat");
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = 8;
        b[66] = 0x29;
        b[67..71].copy_from_slice(&0x1234_abcdu32.to_le_bytes());
        b[71..82].copy_from_slice(label);
        b[82..90].copy_from_slice(b"FAT32   ");
        b[510] = 0x55;
        b[511] = 0xaa;
        b
    }

    #[test]
    fn identifies_volumes() {
        let mut none = |_: u64, _: &mut [u8]| false;
        let fat = identify(&fat32_boot(b"MY STICK   "), &mut none).unwrap();
        assert_eq!(fat, Volume { fstype: "vfat".into(), label: "MY STICK".into(), uuid: "1234-ABCD".into() });
        assert_eq!(identify(&fat32_boot(b"NO NAME    "), &mut none).unwrap().label, "");
        let mut ext = vec![0u8; HEAD_BYTES];
        ext[1024 + 56..1024 + 58].copy_from_slice(&0xef53u16.to_le_bytes());
        ext[1024 + 96..1024 + 100].copy_from_slice(&0x40u32.to_le_bytes());
        ext[1024 + 120..1024 + 124].copy_from_slice(b"data");
        let found = identify(&ext, &mut none).unwrap();
        assert_eq!((found.fstype.as_str(), found.label.as_str()), ("ext4", "data"));
        // exFAT: its label in the root directory.
        let mut exfat = vec![0u8; HEAD_BYTES];
        exfat[3..11].copy_from_slice(b"EXFAT   ");
        exfat[88..92].copy_from_slice(&64u32.to_le_bytes());
        exfat[96..100].copy_from_slice(&4u32.to_le_bytes());
        exfat[100..104].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        exfat[108] = 9;
        exfat[109] = 3;
        let mut root = |offset: u64, buf: &mut [u8]| {
            assert_eq!(offset, 64 * 512 + 2 * 4096);
            buf[0] = 0x83;
            buf[1] = 3;
            for (i, c) in "Cam".encode_utf16().enumerate() {
                buf[2 + i * 2..4 + i * 2].copy_from_slice(&c.to_le_bytes());
            }
            true
        };
        assert_eq!(identify(&exfat, &mut root).unwrap(), Volume { fstype: "exfat".into(), label: "Cam".into(), uuid: "DEAD-BEEF".into() });
        assert_eq!(identify(&vec![0u8; HEAD_BYTES], &mut none), None);
    }

    #[test]
    fn mount_folders_and_types() {
        let media = Path::new("/media/ana");
        let volume = Volume { fstype: "vfat".into(), label: "a/b".into(), uuid: "1-2".into() };
        assert_eq!(mount_folder(media, &volume, "/dev/sda1", &|_| false), Path::new("/media/ana/a_b"));
        let taken = |path: &Path| path == Path::new("/media/ana/a_b");
        assert_eq!(mount_folder(media, &volume, "/dev/sda1", &taken), Path::new("/media/ana/a_b1"));
        let unnamed = Volume { fstype: "vfat".into(), ..Default::default() };
        assert_eq!(mount_folder(media, &unnamed, "/dev/sdb", &|_| false), Path::new("/media/ana/sdb"));
        assert!(mount_type(&volume, 1000, 1000).unwrap().1.contains("uid=1000"));
        let ntfs = mount_type(&Volume { fstype: "ntfs".into(), ..Default::default() }, 1000, 1000).unwrap();
        assert_eq!(ntfs, (NTFS_TYPE, "uid=1000,gid=1000,dmask=0077,fmask=0177,windows_names".to_owned()));
        assert!(mount_type(&Volume { fstype: "btrfs".into(), ..Default::default() }, 0, 0).is_err());
        assert_eq!(valid_device("/dev/sda1"), Some("sda1"));
        assert_eq!(valid_device("/dev/../etc"), None);
        assert_eq!(valid_device("/etc/passwd"), None);
    }

    struct Fake(RefCell<Vec<String>>);

    impl MountOps for Fake {
        fn mount(&mut self, source: &str, target: &Path, fstype: &str, _flags: u64, data: &str) -> io::Result<()> {
            self.0.borrow_mut().push(format!("mount {source} {} {fstype} {data}", target.display()));
            Ok(())
        }
        fn unmount(&mut self, target: &Path) -> io::Result<()> {
            self.0.borrow_mut().push(format!("unmount {}", target.display()));
            Ok(())
        }
    }

    #[test]
    fn answers_requests() {
        let root = std::env::temp_dir().join(format!("osn-drives-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dev")).unwrap();
        fs::write(root.join("dev/sda1"), fat32_boot(b"STICK      ")).unwrap();
        fs::write(root.join("dev/sda"), vec![0u8; HEAD_BYTES]).unwrap();
        fs::write(root.join("dev/nvme0n1p1"), vec![0u8; HEAD_BYTES]).unwrap();
        fs::write(root.join("dev/sdb1"), vec![0u8; HEAD_BYTES]).unwrap();
        fs::write(root.join("partitions"), "major minor #blocks name\n 8 0 1000 sda\n 8 1 900 sda1\n 8 16 3000 sdb\n 8 17 2900 sdb1\n 259 0 5000 nvme0n1\n 259 1 4000 nvme0n1p1\n").unwrap();
        fs::write(root.join("mounts"), "/dev/nvme0n1p1 /data ext2 rw 0 0\n").unwrap();
        // sda: a USB stick; sdb: a SATA disk, by its vendor.
        fs::create_dir_all(root.join("sys/sda/device")).unwrap();
        fs::write(root.join("sys/sda/device/vendor"), "SanDisk\n").unwrap();
        fs::create_dir_all(root.join("sys/sdb/device")).unwrap();
        fs::write(root.join("sys/sdb/device/vendor"), "ATA\n").unwrap();
        let view = SystemView { dev: root.join("dev"), sys_block: root.join("sys"), partitions: root.join("partitions"), mounts: root.join("mounts"), media: root.join("media") };
        let mut ops = Fake(RefCell::new(Vec::new()));
        let list = view.answer("drives", "ana", 1000, 1000, &mut ops);
        assert!(list.starts_with("drives {\"blockdevices\":[{\"name\":\"sda\""), "{list}");
        assert!(list.contains("\"label\":\"STICK\"") && list.contains("\"tran\":\"usb\""));
        assert!(list.contains("\"name\":\"sdb\",\"path\":\"/dev/sdb\",\"size\":3072000,\"type\":\"disk\""), "{list}");
        assert!(list.contains("\"rm\":false,\"hotplug\":false,\"model\":null,\"tran\":\"sata\""), "{list}");
        let answer = view.answer("drives mount /dev/sda1", "ana", 1000, 1000, &mut ops);
        assert_eq!(answer, format!("mounted {}", root.join("media/ana/STICK").display()));
        assert!(ops.0.borrow()[0].contains("vfat uid=1000,gid=1000"));
        assert_eq!(view.answer("drives mount /dev/sda", "ana", 1000, 1000, &mut ops), "failed open one of the drive's partitions");
        assert_eq!(view.answer("drives eject /dev/nvme0n1", "ana", 1000, 1000, &mut ops), "failed the system runs from that drive");
        fs::write(root.join("mounts"), format!("/dev/sda1 {} vfat rw 0 0\n", root.join("media/ana/STICK").display())).unwrap();
        assert_eq!(view.answer("drives eject /dev/sda", "ana", 1000, 1000, &mut ops), "ok");
        assert!(ops.0.borrow().last().unwrap().starts_with("unmount "));
        assert!(!root.join("media/ana/STICK").exists());
        assert_eq!(view.answer("drives mount /dev/../x", "ana", 1000, 1000, &mut ops), "invalid");
        let _ = fs::remove_dir_all(&root);
    }
}
