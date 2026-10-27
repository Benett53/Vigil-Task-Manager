//! Updates from GitHub releases.
//!
//! The only network access in Vigil. It asks GitHub for the latest release at
//! most once a day while that is switched on, and otherwise only when the user
//! asks: the button in the settings or `vigil --update`.
//!
//! Nothing is installed unless two checks pass: `SHA256SUMS` must carry a valid
//! minisign signature from the key compiled into this program, and the
//! downloaded file must match its line in that list. A checksum on its own only
//! catches broken downloads; the signature is what stops a replaced release.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const REPO: &str = "Benett53/Vigil-Task-Manager";
/// minisign public key of the release pipeline (`MINISIGN_SECRET_KEY` secret).
const PUBLIC_KEY: &str = "RWSDt6FfSFgIY3smzlLjBquO9HKZ2SFFAeECiLKAZm2xdAf+CXjbI93q";
const SUMS: &str = "SHA256SUMS.txt";
const SIGNATURE: &str = "SHA256SUMS.txt.minisig";
/// Prefix of the download folders in the temp directory.
const TEMP_PREFIX: &str = "vigil-update-";
/// How long a check counts as recent.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub assets: Vec<Asset>,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(Debug, Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    /// Bytes received and expected size, if known.
    Downloading(u64, Option<u64>),
    /// The new version is being installed; the program is about to quit.
    Installing,
    Failed(String),
}

/// Shared between the UI and the worker threads.
#[derive(Clone)]
pub struct Updater {
    status: Arc<Mutex<Status>>,
    /// File holding the unix time of the last successful check.
    state_file: PathBuf,
    automatic: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    /// Set once an installer or replacement has been started.
    restart: Arc<AtomicBool>,
}

impl Updater {
    /// Starts the background loop that checks once a day while `automatic` is on.
    pub fn new<F>(automatic: bool, state_file: PathBuf, wake: F) -> Self
    where
        F: Fn() + Send + Clone + 'static,
    {
        cleanup();

        let updater = Self {
            status: Arc::default(),
            state_file,
            automatic: Arc::new(AtomicBool::new(automatic)),
            busy: Arc::default(),
            restart: Arc::default(),
        };

        let background = updater.clone();
        let _ = std::thread::Builder::new()
            .name("vigil-update-timer".into())
            .spawn(move || {
                // A short delay keeps the check away from startup work.
                std::thread::sleep(Duration::from_secs(20));
                loop {
                    if background.automatic.load(Ordering::Relaxed) && background.check_due() {
                        background.check(wake.clone());
                    }
                    std::thread::sleep(Duration::from_secs(60 * 60));
                }
            });

        updater
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn set_automatic(&self, on: bool) {
        self.automatic.store(on, Ordering::Relaxed);
    }

    /// The newer release, if one was found.
    pub fn available(&self) -> Option<Release> {
        match self.status() {
            Status::Available(release) => Some(release),
            _ => None,
        }
    }

    /// An installer or the new executable was started and the program should quit.
    pub fn wants_exit(&self) -> bool {
        self.restart.load(Ordering::Relaxed)
    }

    fn check_due(&self) -> bool {
        let last = std::fs::read_to_string(&self.state_file)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(0);
        now().saturating_sub(last) >= CHECK_INTERVAL.as_secs()
    }

    fn set(&self, status: Status) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    /// Ask GitHub for the latest release on a worker thread.
    pub fn check<F: Fn() + Send + 'static>(&self, wake: F) {
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        self.set(Status::Checking);
        wake();
        std::thread::spawn(move || {
            let status = match latest_release() {
                Ok(release) if is_newer(&release.version, CURRENT_VERSION) => {
                    Status::Available(release)
                }
                Ok(_) => Status::UpToDate,
                Err(error) => Status::Failed(format!("Update check failed: {error}")),
            };
            if !matches!(status, Status::Failed(_)) {
                if let Some(dir) = this.state_file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&this.state_file, now().to_string());
            }
            this.set(status);
            this.busy.store(false, Ordering::Release);
            wake();
        });
    }

    /// Download, verify and start installing `release` on a worker thread.
    pub fn install<F: Fn() + Send + Clone + 'static>(&self, release: Release, wake: F) {
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        self.set(Status::Downloading(0, None));
        wake();
        std::thread::spawn(move || {
            let progress = {
                let this = this.clone();
                let wake = wake.clone();
                move |received, total| {
                    this.set(Status::Downloading(received, total));
                    wake();
                }
            };
            match download_and_install(&release, true, progress) {
                Ok(()) => {
                    this.set(Status::Installing);
                    this.restart.store(true, Ordering::Release);
                }
                Err(error) => this.set(Status::Failed(format!("Update failed: {error}"))),
            }
            this.busy.store(false, Ordering::Release);
            wake();
        });
    }
}

/// `vigil --update`: check, and install a newer version if there is one.
/// Returns the process exit code.
pub fn run_cli() -> i32 {
    cleanup();
    println!("Vigil {CURRENT_VERSION}, checking for updates...");
    let release = match latest_release() {
        Ok(release) => release,
        Err(error) => {
            eprintln!("Update check failed: {error}");
            return 1;
        }
    };
    if !is_newer(&release.version, CURRENT_VERSION) {
        println!("Up to date.");
        return 0;
    }
    println!("Installing Vigil {}...", release.version);
    let progress = |received: u64, total: Option<u64>| {
        if let Some(total) = total.filter(|t| *t > 0) {
            print!("\r{} %", received * 100 / total);
            let _ = std::io::stdout().flush();
        }
    };
    match download_and_install(&release, false, progress) {
        Ok(()) => {
            println!("\rUpdated to {}.", release.version);
            0
        }
        Err(error) => {
            eprintln!("\rUpdate failed: {error}");
            1
        }
    }
}

type Result<T> = std::result::Result<T, String>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(concat!("Vigil/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn latest_release() -> Result<Release> {
    #[derive(serde::Deserialize)]
    struct ApiAsset {
        name: String,
        browser_download_url: String,
    }
    #[derive(serde::Deserialize)]
    struct ApiRelease {
        tag_name: String,
        assets: Vec<ApiAsset>,
    }

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let text = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let api: ApiRelease = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    Ok(Release {
        version: api.tag_name.trim_start_matches('v').to_string(),
        assets: api
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
            })
            .collect(),
    })
}

/// `1.10.0` is newer than `1.9.3`. Anything that does not parse is not newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(version: &str) -> Option<(u64, u64, u64)> {
        let core = version.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        Some((
            parts.next()??,
            parts.next()??,
            parts.next().unwrap_or(Some(0))?,
        ))
    }
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn fetch_text(url: &str) -> Result<String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// The checksum list, but only if its signature is valid.
fn verified_sums(release: &Release) -> Result<String> {
    let sums_asset = release
        .asset(SUMS)
        .ok_or("the release has no checksum list")?;
    let sig_asset = release
        .asset(SIGNATURE)
        .ok_or("the release is not signed")?;
    let sums = fetch_text(&sums_asset.url)?;
    let signature = fetch_text(&sig_asset.url)?;
    verify_signature(sums.as_bytes(), &signature)?;
    Ok(sums)
}

fn verify_signature(data: &[u8], signature: &str) -> Result<()> {
    let key = minisign_verify::PublicKey::from_base64(PUBLIC_KEY).map_err(|e| e.to_string())?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|e| e.to_string())?;
    key.verify(data, &signature, false)
        .map_err(|_| "the signature of the checksum list is not valid".to_string())
}

/// The expected SHA-256 of `name` in a `sha256sum` style list.
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Stream `url` into `dest`, returning the SHA-256 of what was written.
fn download(url: &str, dest: &Path, progress: &impl Fn(u64, Option<u64>)) -> Result<String> {
    let response = agent().get(url).call().map_err(|e| e.to_string())?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    let mut reported = 0u64;
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        received += n as u64;
        if received - reported > 256 * 1024 {
            reported = received;
            progress(received, total);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Download the right file for this copy, check it, and hand over to it.
/// `restart` starts the new version afterwards; the command line does not.
fn download_and_install(
    release: &Release,
    restart: bool,
    progress: impl Fn(u64, Option<u64>),
) -> Result<()> {
    let plan = plan(release)?;
    let asset = release
        .asset(&plan.asset)
        .ok_or_else(|| format!("{} is missing from the release", plan.asset))?;
    let sums = verified_sums(release)?;
    let expected = expected_hash(&sums, &plan.asset)
        .ok_or_else(|| format!("{} is not in the checksum list", plan.asset))?;

    let dir = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}", release.version));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(&plan.asset);
    let actual = download(&asset.url, &file, &progress)?;
    if actual != expected {
        let _ = std::fs::remove_file(&file);
        return Err("the downloaded file does not match its checksum".into());
    }

    install(&plan, &file, &dir, restart)
}

/// Removes what an earlier update left behind.
fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

// --- Vigil specifics ---------------------------------------------------------

struct Plan {
    asset: String,
    method: Method,
}

enum Method {
    /// Run the setup silently; it starts the new version when asked to.
    #[cfg(windows)]
    Installer { all_users: bool },
    /// A copy run from an unpacked archive: swap the executable in place.
    Portable,
    /// Installed from the .deb package.
    #[cfg(target_os = "linux")]
    Deb,
    /// Installed from the .rpm package.
    #[cfg(target_os = "linux")]
    Rpm,
    /// Installed with the `install.sh` of the release archive.
    #[cfg(target_os = "linux")]
    InstallScript { system: bool },
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| e.to_string())
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

#[cfg(windows)]
fn plan(release: &Release) -> Result<Plan> {
    let version = &release.version;
    let exe = current_exe()?;
    let dir = exe.parent().ok_or("no program folder")?;
    // Inno Setup leaves its uninstaller next to the program.
    if dir.join("unins000.exe").exists() {
        let all_users = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .is_some_and(|pf| dir.starts_with(pf));
        Ok(Plan {
            asset: format!("VigilSetup-{version}.exe"),
            method: Method::Installer { all_users },
        })
    } else {
        Ok(Plan {
            asset: format!("vigil-{version}-windows-x86_64.zip"),
            method: Method::Portable,
        })
    }
}

#[cfg(target_os = "linux")]
fn plan(release: &Release) -> Result<Plan> {
    let version = &release.version;
    let (arch, deb_arch) = match std::env::consts::ARCH {
        "x86_64" => ("x86_64", "amd64"),
        "aarch64" => ("aarch64", "arm64"),
        other => return Err(format!("there is no release for {other}")),
    };
    let exe = current_exe()?;
    let owned_by = |tool: &str, args: &[&str]| {
        std::process::Command::new(tool)
            .args(args)
            .arg(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let tarball = format!("vigil-{version}-linux-{arch}.tar.gz");

    if exe.starts_with("/usr/bin") {
        if owned_by("dpkg", &["-S"]) {
            return Ok(Plan {
                asset: format!("vigil-task-manager_{version}-1_{deb_arch}.deb"),
                method: Method::Deb,
            });
        }
        if owned_by("rpm", &["-qf"]) {
            return Ok(Plan {
                asset: format!("vigil-task-manager-{version}-1.{arch}.rpm"),
                method: Method::Rpm,
            });
        }
        return Err("Vigil was installed by the package manager; update it there".into());
    }
    let home_bin = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin"));
    if home_bin.is_some_and(|bin| exe.starts_with(bin)) {
        return Ok(Plan {
            asset: tarball,
            method: Method::InstallScript { system: false },
        });
    }
    if exe.starts_with("/usr/local/bin") {
        return Ok(Plan {
            asset: tarball,
            method: Method::InstallScript { system: true },
        });
    }
    Ok(Plan {
        asset: tarball,
        method: Method::Portable,
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
fn plan(_release: &Release) -> Result<Plan> {
    Err("updates are not supported on this system".into())
}

fn install(plan: &Plan, file: &Path, dir: &Path, restart: bool) -> Result<()> {
    match plan.method {
        #[cfg(windows)]
        Method::Installer { all_users } => return run_installer(file, all_users, restart),
        Method::Portable => {
            let unpacked = unpack(file, dir)?;
            replace_executable(&unpacked)?;
        }
        #[cfg(target_os = "linux")]
        Method::Deb => {
            let installed = if has("apt-get") {
                run_privileged(&["apt-get", "install", "-y", "--allow-downgrades"], file)
            } else {
                run_privileged(&["dpkg", "-i"], file)
            };
            installed?;
        }
        #[cfg(target_os = "linux")]
        Method::Rpm => {
            let installed = if has("dnf") {
                run_privileged(&["dnf", "install", "-y"], file)
            } else if has("zypper") {
                run_privileged(
                    &[
                        "zypper",
                        "--non-interactive",
                        "install",
                        "--allow-unsigned-rpm",
                    ],
                    file,
                )
            } else {
                run_privileged(&["rpm", "-U"], file)
            };
            installed?;
        }
        #[cfg(target_os = "linux")]
        Method::InstallScript { system } => {
            let unpacked = unpack(file, dir)?;
            let script = unpacked.with_file_name("install.sh");
            if system {
                let status = std::process::Command::new("pkexec")
                    .arg("sh")
                    .arg(&script)
                    .arg("--system")
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("the installation was cancelled or failed".into());
                }
            } else {
                let status = std::process::Command::new("sh")
                    .arg(&script)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("install.sh failed".into());
                }
            }
        }
    }
    if restart {
        std::process::Command::new(current_exe()?)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The system `tar`. On Windows the one in System32 (bsdtar, reads zip files),
/// not whichever `tar` comes first on the PATH.
fn tar() -> PathBuf {
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let tar = PathBuf::from(root).join("System32").join("tar.exe");
        if tar.is_file() {
            return tar;
        }
    }
    PathBuf::from("tar")
}

/// Unpack a release archive with the system `tar` (which reads zip files on
/// Windows too) and return the path of the executable inside.
fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf> {
    let target = dir.join("unpacked");
    let _ = std::fs::remove_dir_all(&target);
    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    let status = std::process::Command::new(tar())
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&target)
        .status()
        .map_err(|e| format!("tar could not be started: {e}"))?;
    if !status.success() {
        return Err("the archive could not be unpacked".into());
    }
    let name = if cfg!(windows) { "vigil.exe" } else { "vigil" };
    let folder = std::fs::read_dir(&target)
        .map_err(|e| e.to_string())?
        .flatten()
        .find(|entry| entry.path().join(name).is_file())
        .ok_or("the archive does not contain Vigil")?;
    Ok(folder.path().join(name))
}

#[cfg(target_os = "linux")]
fn has(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

/// Run `command file` as root; the desktop asks for the password.
#[cfg(target_os = "linux")]
fn run_privileged(command: &[&str], file: &Path) -> Result<()> {
    let status = std::process::Command::new("pkexec")
        .args(command)
        .arg(file)
        .status()
        .map_err(|e| format!("administrator rights are needed ({e})"))?;
    if status.success() {
        Ok(())
    } else {
        Err("the installation was cancelled or failed".into())
    }
}

/// Start the setup once this copy has had time to quit, so the setup never
/// finds Vigil still running.
#[cfg(windows)]
fn run_installer(setup: &Path, all_users: bool, restart: bool) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // For a Program Files installation the setup asks for administrator
    // rights itself, which also lets it start Vigil again without them.
    let scope = if all_users {
        "/ALLUSERS"
    } else {
        "/CURRENTUSER"
    };
    let restart = if restart { " /restartapp=1" } else { "" };
    let command = format!(
        "ping -n 3 127.0.0.1 >nul & start \"\" \"{}\" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART {scope}{restart}",
        setup.display()
    );
    std::process::Command::new("cmd")
        .args(["/d", "/c"])
        .raw_arg(command)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Swap the running executable for `new`.
///
/// A running executable cannot be overwritten on Windows but can be renamed,
/// so the old file moves aside and is removed on the next start. On Linux the
/// new file is renamed over the old one, which leaves the running copy alone.
fn replace_executable(new: &Path) -> Result<()> {
    let exe = current_exe()?;
    #[cfg(windows)]
    {
        let old = old_path(&exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
        if let Err(error) = std::fs::copy(new, &exe) {
            let _ = std::fs::rename(&old, &exe);
            return Err(error.to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let staged = exe.with_file_name(".vigil-update");
        std::fs::copy(new, &staged)
            .and_then(|_| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)))
            .and_then(|_| std::fs::rename(&staged, &exe))
            .map_err(|e| {
                let _ = std::fs::remove_file(&staged);
                format!("the program file could not be replaced ({e})")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("garbage", "0.4.0"));
    }

    #[test]
    fn finds_hashes_in_both_sha256sum_formats() {
        let sums = "abc123  vigil.zip\nDEF456 *VigilSetup-0.4.0.exe\n";
        assert_eq!(expected_hash(sums, "vigil.zip").as_deref(), Some("abc123"));
        assert_eq!(
            expected_hash(sums, "VigilSetup-0.4.0.exe").as_deref(),
            Some("def456")
        );
        assert_eq!(expected_hash(sums, "other"), None);
    }

    /// Signed with the release key, so this also proves `PUBLIC_KEY` matches it.
    const SIGNED: &[u8] = b"abc123  vigil.zip\n";
    const SIGNATURE_OF_SIGNED: &str = include_str!("../tests/update-signature.minisig");

    #[test]
    fn accepts_a_release_signature() {
        assert!(verify_signature(SIGNED, SIGNATURE_OF_SIGNED).is_ok());
    }

    #[test]
    fn rejects_a_changed_list() {
        assert!(verify_signature(b"abc124  vigil.zip\n", SIGNATURE_OF_SIGNED).is_err());
    }

    /// Checks a real release end to end, without installing anything:
    /// `cargo test -- --ignored latest_release_verifies`.
    #[test]
    #[ignore = "needs the network"]
    fn latest_release_verifies() {
        let release = latest_release().expect("release");
        let sums = verified_sums(&release).expect("signed checksums");
        let plan = plan(&release).expect("plan");
        let asset = release.asset(&plan.asset).expect("asset");
        let dir = std::env::temp_dir().join("vigil-update-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(&plan.asset);
        let actual = download(&asset.url, &file, &|_, _| {}).expect("download");
        assert_eq!(Some(actual), expected_hash(&sums, &plan.asset));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
