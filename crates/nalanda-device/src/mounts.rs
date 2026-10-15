use std::path::PathBuf;

/// Directories that might be the root of a removable volume.
pub(crate) fn candidate_mount_points() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    if let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") {
        out.extend(parse_proc_mounts(&mounts));
    }
    #[cfg(target_os = "macos")]
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        out.extend(entries.flatten().map(|e| e.path()));
    }
    #[cfg(windows)]
    out.extend((b'D'..=b'Z').map(|d| PathBuf::from(format!("{}:\\", d as char))));
    out.sort();
    out.dedup();
    out
}

/// Mount points from `/proc/mounts`, skipping virtual filesystems.
/// Paths are octal-escaped there (`\040` for a space).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_mounts(contents: &str) -> Vec<PathBuf> {
    const VIRTUAL: [&str; 12] = [
        "proc",
        "sysfs",
        "devtmpfs",
        "devpts",
        "tmpfs",
        "cgroup",
        "cgroup2",
        "overlay",
        "securityfs",
        "debugfs",
        "mqueue",
        "squashfs",
    ];
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mount = fields.next()?;
            let fstype = fields.next()?;
            (!VIRTUAL.contains(&fstype)).then(|| PathBuf::from(unescape_octal(mount)))
        })
        .collect()
}

fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 4 <= bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let v = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_mounts() {
        let mounts = "proc /proc proc rw 0 0\n\
                      /dev/sda1 / ext4 rw 0 0\n\
                      /dev/sdb /media/me/KOBOeReader vfat rw 0 0\n\
                      /dev/sdc /media/me/My\\040Kobo vfat rw 0 0\n\
                      tmpfs /run tmpfs rw 0 0\n";
        assert_eq!(
            parse_proc_mounts(mounts),
            [
                PathBuf::from("/"),
                PathBuf::from("/media/me/KOBOeReader"),
                PathBuf::from("/media/me/My Kobo"),
            ]
        );
    }
}
