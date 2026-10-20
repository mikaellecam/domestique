//! Updates from the latest GitHub release. Its SHA256SUMS is signed with
//! minisign, and the archive must match it before anything installed changes.

use domestique::Result;
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs, io};

pub const RELEASES: &str = "https://github.com/mikaellecam/domestique/releases";
const LATEST: &str = "https://api.github.com/repos/mikaellecam/domestique/releases/latest";
const KEY: &str = include_str!("../../../../minisign.pub");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const EXE: &str = if cfg!(windows) {
    "domestique.exe"
} else {
    "domestique"
};
const CLI: &str = if cfg!(windows) {
    "domestique-cli.exe"
} else {
    "domestique-cli"
};
/// Beside the install, so that the new files only need renaming into place.
const STAGING: &str = ".domestique-update";

/// This computer's archive, as release.yml names it.
const ARCHIVE: Option<&str> = if cfg!(all(windows, target_arch = "x86_64")) {
    Some("domestique-windows-x86_64.zip")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("domestique-linux-x86_64.tar.gz")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("domestique-macos-arm64.zip")
} else {
    None
};

#[derive(Clone, PartialEq, Deserialize)]
pub struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    /// "0.2.0"
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }

    fn url(&self, name: &str) -> Result<&str> {
        match self.assets.iter().find(|a| a.name == name) {
            Some(asset) => Ok(&asset.browser_download_url),
            None => Err(format!("the release has no {name}").into()),
        }
    }
}

/// The latest release, if it is newer than this one and has a signed archive
/// for this computer.
pub fn check() -> Result<Option<Release>> {
    let Some(archive) = ARCHIVE else {
        return Ok(None);
    };
    let latest = setting("DOMESTIQUE_UPDATE_URL", LATEST);
    let release: Release = match get(&latest)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r.into_body().read_json()?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None), // nothing released yet
        Err(e) => return Err(e.into()),
    };
    let complete = [archive, "SHA256SUMS", "SHA256SUMS.minisig"]
        .iter()
        .all(|name| release.url(name).is_ok());
    Ok((complete && newer(&release.tag_name, VERSION)).then_some(release))
}

/// Downloads and checks `release`, puts it in place of this install and
/// starts it. The tray quits next.
pub fn install(release: &Release) -> Result<()> {
    let archive = ARCHIVE.ok_or("no updates for this computer")?;
    let exe = env::current_exe()?;
    let (dir, moves) = plan(&exe);
    let staging = dir.join(STAGING);
    remove(&staging);
    fs::create_dir(&staging).map_err(|e| {
        let dir = dir.display();
        format!("can't write to {dir} ({e}). Download the new version from {RELEASES}")
    })?;
    let result = stage(release, archive, &staging).and_then(|()| Ok(swap(&moves)?));
    remove(&staging);
    result?;
    Command::new(&exe).arg("--updated").spawn()?;
    Ok(())
}

/// Deletes what an update of the install at `exe` left: the replaced files
/// and the staging folder.
pub fn clean_up(exe: &Path) {
    let (dir, moves) = plan(exe);
    for (_, installed) in moves {
        remove(&old(&installed));
    }
    remove(&dir.join(STAGING));
}

/// Whether `tag`, "vX.Y.Z", is a later version than `version`, "X.Y.Z".
fn newer(tag: &str, version: &str) -> bool {
    let triple = |s: &str| -> Option<(u64, u64, u64)> {
        let mut n = s.split('.').map(|n| n.parse().ok());
        let t = (n.next()??, n.next()??, n.next()??);
        n.next().is_none().then_some(t)
    };
    match (tag.strip_prefix('v').and_then(triple), triple(version)) {
        (Some(t), Some(v)) => t > v,
        _ => false,
    }
}

/// Debug builds can take the latest release's address and the public key from
/// the environment, to try updates against a local server. Release builds can't.
#[cfg(debug_assertions)]
fn setting(var: &str, release: &str) -> String {
    env::var(var).unwrap_or_else(|_| release.to_owned())
}

#[cfg(not(debug_assertions))]
fn setting(_: &str, release: &str) -> String {
    release.to_owned()
}

fn get(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    ureq::get(url).header(
        "User-Agent",
        concat!("domestique/", env!("CARGO_PKG_VERSION")),
    )
}

/// At most `limit` bytes.
fn download(url: &str, limit: u64) -> Result<Vec<u8>> {
    let body = get(url).call()?.into_body();
    Ok(body.into_with_config().limit(limit).read_to_vec()?)
}

/// Downloads the archive into `dir` and extracts it there, once SHA256SUMS
/// is signed for this release and lists the archive's SHA-256.
fn stage(release: &Release, archive: &str, dir: &Path) -> Result<()> {
    let sums = download(release.url("SHA256SUMS")?, 64 << 10)?;
    let sig = download(release.url("SHA256SUMS.minisig")?, 4 << 10)?;
    let key = setting("DOMESTIQUE_UPDATE_KEY", KEY);
    verify(&key, &sums, &sig, &release.tag_name)?;
    let listed = sha256_of(std::str::from_utf8(&sums)?, archive)
        .ok_or(format!("SHA256SUMS doesn't list {archive}"))?;
    let data = download(release.url(archive)?, 100 << 20)?;
    if !sha256(&data).eq_ignore_ascii_case(listed) {
        return Err(format!("{archive} doesn't match SHA256SUMS").into());
    }
    let path = dir.join(archive);
    fs::write(&path, data)?;
    extract(&path, dir)
}

/// Checks that `key` signed `sums` for the release `tag`.
fn verify(key: &str, sums: &[u8], sig: &[u8], tag: &str) -> Result<()> {
    let sig = Signature::decode(&String::from_utf8_lossy(sig))?;
    let key = PublicKey::decode(key)?;
    key.verify(sums, &sig, false)
        .map_err(|e| format!("SHA256SUMS: {e}"))?;
    // Else an older release could pass for this one.
    let comment = sig.trusted_comment();
    if comment != format!("domestique {tag}") {
        return Err(format!("SHA256SUMS is signed as {comment:?}, not as {tag}").into());
    }
    Ok(())
}

/// The SHA-256 that `sums`, as sha256sum writes them, lists for `name`.
fn sha256_of<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(' ')?;
        (file.strip_prefix([' ', '*'])? == name).then_some(hash)
    })
}

fn sha256(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The OS's own tar, which reads zips too except on Linux.
fn tar() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let root = env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let mut tar = Command::new(Path::new(&root).join(r"System32\tar.exe"));
        tar.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
        tar
    }
    #[cfg(not(windows))]
    Command::new(if cfg!(target_os = "macos") {
        "/usr/bin/tar"
    } else {
        "tar"
    })
}

fn extract(archive: &Path, dir: &Path) -> Result<()> {
    let flags = if cfg!(target_os = "linux") {
        "-xzf"
    } else {
        "-xf"
    };
    let out = tar().arg(flags).arg(archive).arg("-C").arg(dir).output()?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        return Err(format!("can't extract the update: {}", e.trim()).into());
    }
    Ok(())
}

/// The folder `exe` is installed in, and the moves that update it from the
/// staging folder there: the app, which is its bundle on macOS, then the
/// command line beside it if there is one.
fn plan(exe: &Path) -> (PathBuf, Vec<(PathBuf, PathBuf)>) {
    let bundle = exe.ancestors().nth(3).filter(|b| {
        cfg!(target_os = "macos")
            && b.extension().is_some_and(|e| e == "app")
            && exe.parent().is_some_and(|p| p.ends_with("Contents/MacOS"))
    });
    let (app, name) = match bundle {
        Some(bundle) => (bundle, "Domestique.app"),
        None => (exe, EXE),
    };
    let dir = app.parent().unwrap_or(Path::new("."));
    let staging = dir.join(STAGING);
    let mut moves = vec![(staging.join(name), app.to_owned())];
    if dir.join(CLI).exists() {
        moves.push((staging.join(CLI), dir.join(CLI)));
    }
    (dir.to_owned(), moves)
}

/// Moves each new file or bundle over the installed one, which is kept as
/// `<name>.old`. On a failure, moves back what it moved.
fn swap(moves: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    for (i, (new, installed)) in moves.iter().enumerate() {
        remove(&old(installed));
        let moved = fs::rename(installed, old(installed)).and_then(|()| {
            fs::rename(new, installed).inspect_err(|_| {
                let _ = fs::rename(old(installed), installed);
            })
        });
        if let Err(e) = moved {
            for (new, installed) in moves[..i].iter().rev() {
                let _ = fs::rename(installed, new);
                let _ = fs::rename(old(installed), installed);
            }
            return Err(e);
        }
    }
    Ok(())
}

fn old(path: &Path) -> PathBuf {
    let mut old = path.as_os_str().to_owned();
    old.push(".old");
    old.into()
}

/// Deletes a file or folder, if it can.
fn remove(path: &Path) {
    let _ = fs::remove_file(path).or_else(|_| fs::remove_dir_all(path));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &[u8] = include_bytes!("../../../../tests/update/SHA256SUMS");
    const SIG: &[u8] = include_bytes!("../../../../tests/update/SHA256SUMS.minisig");
    const TEST_KEY: &str = include_str!("../../../../tests/update/test.pub");
    const OTHER_KEY: &str = include_str!("../../../../tests/update/other.pub");

    /// An empty folder of its own under the system's temp folder.
    fn temp(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("domestique-update-{name}-{}", std::process::id()));
        remove(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(path: impl AsRef<Path>) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn versions() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("v0.1.0", "0.2.0"));
        for odd in ["0.2.0", "v0.2", "v0.2.0.1", "v0.2.0-rc1", "vx.y.z", ""] {
            assert!(!newer(odd, "0.1.0"), "{odd}");
        }
    }

    #[test]
    fn sums() {
        let sums = "aa11  domestique-linux-x86_64.tar.gz\nbb22 *x.zip\ncc33  a x.zip\n";
        assert_eq!(
            sha256_of(sums, "domestique-linux-x86_64.tar.gz"),
            Some("aa11")
        );
        assert_eq!(sha256_of(sums, "x.zip"), Some("bb22"));
        assert_eq!(sha256_of(sums, "a x.zip"), Some("cc33"));
        assert_eq!(sha256_of(sums, "domestique-linux-x86_64"), None);
        assert_eq!(sha256_of(sums, ""), None);
        // As sha256sum wrote it, for an "archive" holding "linux".
        let fixture = std::str::from_utf8(SUMS).unwrap();
        let listed = sha256_of(fixture, "domestique-linux-x86_64.tar.gz");
        assert_eq!(listed, Some(sha256(b"linux").as_str()));
    }

    #[test]
    fn signatures() {
        verify(TEST_KEY, SUMS, SIG, "v0.2.0").unwrap();
        let mut tampered = SUMS.to_vec();
        tampered[0] ^= 1;
        assert!(verify(TEST_KEY, &tampered, SIG, "v0.2.0").is_err());
        let replayed = verify(TEST_KEY, SUMS, SIG, "v0.3.0").unwrap_err();
        assert!(replayed.to_string().contains("not as v0.3.0"), "{replayed}");
        assert!(verify(OTHER_KEY, SUMS, SIG, "v0.2.0").is_err());
        assert!(verify(TEST_KEY, SUMS, b"", "v0.2.0").is_err());
        // The key the app is built with.
        PublicKey::decode(KEY).unwrap();
    }

    /// Writes `files` in `dir` with "old" in them, and in its staging folder
    /// with "new".
    fn install_and_stage(dir: &Path, files: &[&str]) {
        fs::create_dir_all(dir.join(STAGING)).unwrap();
        for f in files {
            fs::write(dir.join(f), "old").unwrap();
            fs::write(dir.join(STAGING).join(f), "new").unwrap();
        }
    }

    #[test]
    fn swaps() {
        let dir = temp("swap");
        install_and_stage(&dir, &[EXE, CLI]);
        fs::write(old(&dir.join(EXE)), "older").unwrap();
        let (root, moves) = plan(&dir.join(EXE));
        assert_eq!(root, dir);
        assert_eq!(moves.len(), 2);
        swap(&moves).unwrap();
        for f in [EXE, CLI] {
            assert_eq!(read(dir.join(f)), "new");
            assert_eq!(read(old(&dir.join(f))), "old");
        }
        clean_up(&dir.join(EXE));
        assert!(!old(&dir.join(EXE)).exists() && !dir.join(STAGING).exists());
        remove(&dir);
    }

    #[test]
    fn swaps_without_the_command_line() {
        let dir = temp("swap-app");
        install_and_stage(&dir, &[EXE]);
        let (_, moves) = plan(&dir.join(EXE));
        assert_eq!(moves.len(), 1);
        swap(&moves).unwrap();
        assert_eq!(read(dir.join(EXE)), "new");
        remove(&dir);
    }

    #[test]
    fn rolls_back() {
        let dir = temp("rollback");
        install_and_stage(&dir, &[EXE, CLI]);
        fs::remove_file(dir.join(STAGING).join(CLI)).unwrap();
        let (_, moves) = plan(&dir.join(EXE));
        assert!(swap(&moves).is_err());
        for f in [EXE, CLI] {
            assert_eq!(read(dir.join(f)), "old");
            assert!(!old(&dir.join(f)).exists());
        }
        assert_eq!(read(dir.join(STAGING).join(EXE)), "new");
        remove(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swaps_the_bundle() {
        let dir = temp("bundle");
        let exe = Path::new("Contents/MacOS/domestique");
        let installed = dir.join("My Domestique.app");
        let new = dir.join(STAGING).join("Domestique.app");
        for (bundle, content) in [(&installed, "old"), (&new, "new")] {
            fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
            fs::write(bundle.join(exe), content).unwrap();
        }
        install_and_stage(&dir, &[CLI]);
        let (root, moves) = plan(&installed.join(exe));
        assert_eq!(root, dir);
        swap(&moves).unwrap();
        assert_eq!(read(installed.join(exe)), "new");
        assert_eq!(read(old(&installed).join(exe)), "old");
        assert_eq!(read(dir.join(CLI)), "new");
        clean_up(&installed.join(exe));
        assert!(!old(&installed).exists());
        remove(&dir);
    }

    #[test]
    fn extracts_with_the_system_tar() {
        let dir = temp("extract");
        fs::create_dir(dir.join("in")).unwrap();
        fs::write(dir.join("in").join(EXE), "new").unwrap();
        let (archive, create) = if cfg!(target_os = "linux") {
            (dir.join("a.tar.gz"), ["-czf"].as_slice())
        } else {
            (dir.join("a.zip"), ["--format", "zip", "-cf"].as_slice())
        };
        let made = tar()
            .args(create)
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("in"))
            .arg(EXE)
            .status();
        assert!(made.unwrap().success());
        fs::create_dir(dir.join("out")).unwrap();
        extract(&archive, &dir.join("out")).unwrap();
        assert_eq!(read(dir.join("out").join(EXE)), "new");
        assert!(extract(&dir.join("in").join(EXE), &dir.join("out")).is_err());
        remove(&dir);
    }
}
