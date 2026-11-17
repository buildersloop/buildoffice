//! 保管庫にしてよい場所かの判定。
//!
//! - `check_root`：ファイルを読まない純粋な関数。`RootEnv`（ホーム・設定やシステムの場所）と比べる。
//! - `RootEnv`：環境変数の値・既定の値と、Windows では OS に問い合わせた既知のフォルダの値から作る。どれも足すだけで外さない（どれか1つが本物なら守れる）。値は、文字だけで整えた形を
//!   いつも入れ、開いて整えた形（リンクをたどった最終のパス）も、開けて違うときに足す。テストでの差し替えは、
//!   `RootEnv` を引数で渡すことでだけ行う（根の判定を外す引数・環境変数は作らない）。
//! - 入れ子の保管庫の判定（祖先だけ）と、入力のパスの要素の判定（`.`・`..`、Windows での末尾の `.`・空白、
//!   予約名）は、別の関数にする。

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};

use crate::error::{ErrorCode, Result, VaultError};
use crate::guard;
use crate::relpath::{RESERVED_NAME, is_windows_reserved_name};

/// 保管庫の印のファイル（`.vault/vault.json`）の名前。
pub(crate) const VAULT_JSON: &str = "vault.json";

#[cfg(windows)]
const HOME_VAR: &str = "USERPROFILE";
#[cfg(not(windows))]
const HOME_VAR: &str = "HOME";

/// 根の判定で比べる場所の一覧（ホームと、それ自身・祖先・子孫を拒否する場所）。
#[derive(Debug, Clone)]
pub struct RootEnv {
    /// ホーム（環境変数の値と、Windows では OS の値。それぞれの形）。どれにも、ホームとその祖先・ホームの直下の
    /// `.` の決まりを当てる。
    homes: Vec<PathBuf>,
    protected: Vec<PathBuf>,
}

/// OS に問い合わせた既知のフォルダの場所。
///
/// 3つの状態で持つ：問い合わせていない（`NotAsked`）、問い合わせて取れた（`Asked` の中の `Some`）、問い合わせて
/// 取れなかった（`Asked` の中の `None`）。問い合わせを必須にするのは、Windows の `RootEnv::from_system` だけ
/// （環境変数だけで作るテストと、Windows 以外を壊さない）。
#[derive(Debug, Clone, Default)]
pub(crate) enum OsPlaces {
    /// 問い合わせていない。
    #[default]
    NotAsked,
    /// 問い合わせた。
    #[cfg_attr(not(windows), allow(dead_code))]
    Asked(Box<KnownPlaces>),
}

/// 既知のフォルダの、OS の値（取れなかったものは None）。
#[derive(Debug, Clone, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct KnownPlaces {
    pub(crate) profile: Option<PathBuf>,
    pub(crate) roaming_app_data: Option<PathBuf>,
    pub(crate) local_app_data: Option<PathBuf>,
    pub(crate) documents: Option<PathBuf>,
    pub(crate) windows: Option<PathBuf>,
    pub(crate) program_files: Option<PathBuf>,
    pub(crate) program_files_x86: Option<PathBuf>,
    pub(crate) program_data: Option<PathBuf>,
}

impl RootEnv {
    /// この PC の環境変数と、Windows では OS に問い合わせた既知のフォルダの場所から作る。ホームがわからなければ
    /// `INVALID_ROOT`（環境変数のホームがないときも、Windows で OS にホームを問い合わせられないときも）。
    ///
    /// Windows では、自分のプロセスのトークンで問い合わせるので、スレッドが偽装しているあいだは呼ばない（偽装の
    /// 前か、偽装を解いてから呼ぶ）。
    pub fn from_system() -> Result<Self> {
        Self::from_sources(&|name| std::env::var_os(name), guard::os_places())
    }

    /// 環境変数を読む関数を受け取って作る（OS には問い合わせない。テストで偽の値を渡す）。
    #[cfg(test)]
    pub(crate) fn from_lookup(get: &dyn Fn(&str) -> Option<OsString>) -> Result<Self> {
        Self::from_sources(get, OsPlaces::NotAsked)
    }

    /// 環境変数を読む関数と、OS の値から作る（テストでは、どちらも偽の値を渡す）。
    pub(crate) fn from_sources(
        get: &dyn Fn(&str) -> Option<OsString>,
        os: OsPlaces,
    ) -> Result<Self> {
        // 環境変数のホームがないときは、OS のホームが取れても断る（今までどおり。設計の決定）
        let env_home = absolute_var(get, HOME_VAR).ok_or_else(|| {
            invalid_root(
                "ホームのフォルダがわからない（環境変数が空か、絶対パスでない）ので、保管庫を作ることも開くこともできない",
            )
        })?;
        let os = match os {
            OsPlaces::NotAsked => None,
            OsPlaces::Asked(places) => {
                if places.profile.is_none() {
                    return Err(invalid_root(
                        "OS にホームのフォルダを問い合わせられないので、保管庫を作ることも開くこともできない",
                    ));
                }
                Some(places)
            }
        };
        let mut homes = Places::default();
        homes.opened(&env_home);
        if let Some(home) = os.as_ref().and_then(|os| os.profile.as_ref()) {
            homes.opened(home);
        }
        let homes = homes.0;
        let protected = protected_places(get, &homes, os.as_deref()).0;
        Ok(Self { homes, protected })
    }
}

/// 比べる場所の並び（同じ場所は1つだけ。比べ方は `keys`）。
#[derive(Default)]
struct Places(Vec<PathBuf>);

impl Places {
    /// 文字だけで整えた形を入れる（ファイルに触れない）。
    #[cfg_attr(not(windows), allow(dead_code))]
    fn text(&mut self, path: &Path) {
        self.push(lexical(path));
    }

    /// 文字だけで整えた形と、開いて整えた形（`forms`）を入れる。
    fn opened(&mut self, path: &Path) {
        for form in forms(path) {
            self.push(form);
        }
    }

    fn push(&mut self, path: PathBuf) {
        let key = keys(&path);
        if !self.0.iter().any(|p| keys(p) == key) {
            self.0.push(path);
        }
    }
}

/// 場所の形：開いて整えた形（リンクをたどった最終のパス。開けたときだけ）と、文字だけで整えた形（いつも）。
/// 同じ場所なら、並びに入れるときに1つにまとまる（開いて整えた形が残る）。ファイルに触れるのは、ドライブ文字の
/// パス（Windows）か絶対パス（ほか）だけ。UNC・デバイスの形は文字だけ（ネットワークや装置に触れない）。
fn forms(path: &Path) -> Vec<PathBuf> {
    let text = lexical(path);
    let mut forms = Vec::with_capacity(2);
    if may_open(&text)
        && let Ok(resolved) = guard::resolve_root_path(&text)
    {
        forms.push(resolved);
    }
    forms.push(text);
    forms
}

/// 開いて整えてよい形か：Windows はドライブ文字のパスだけ、ほかは絶対パス。
fn may_open(path: &Path) -> bool {
    match path.components().next() {
        Some(Component::Prefix(p)) => matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
        Some(Component::RootDir) => !cfg!(windows),
        _ => false,
    }
}

/// 環境変数の値。空のとき、または絶対パスでないときは `None`（呼ぶ側が既定値を使う）。
fn absolute_var(get: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    let value = get(name)?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

/// それ自身・祖先・子孫を拒否する場所（Windows）。
///
/// 環境変数の値・既定値・OS の値（問い合わせたとき）を、取れたものはどれも入れる（どれかを差し替えられても、
/// ほかの値の場所は守る）。開いて整えるのは、ホーム（とホームのドキュメント）・AppData・Windows・ProgramFiles・
/// ProgramData から作る値。OS のドキュメントの値と OneDrive の値から作るもの（その下の PowerShell の2つ）は、
/// 文字だけで整える（ドキュメントをネットワークの場所へ移した PC で、init・open のたびにネットワークに触れて
/// 長く待たないため。ホームのドキュメントは、点検で開いて整える形に戻した）。
#[cfg(windows)]
fn protected_places(
    get: &dyn Fn(&str) -> Option<OsString>,
    homes: &[PathBuf],
    os: Option<&KnownPlaces>,
) -> Places {
    let mut list = Places::default();
    // PowerShell のプロファイルのフォルダ（PowerShell の起動時に動くので、自動起動のフォルダと同じく守る）。
    // OS のドキュメントの値と OneDrive の値から作るものは、文字だけで整える
    let powershell = |list: &mut Places, documents: &Path| {
        list.text(&documents.join("WindowsPowerShell"));
        list.text(&documents.join("PowerShell"));
    };
    for home in homes {
        let app_data = home.join("AppData");
        list.opened(&app_data);
        list.opened(&app_data.join("Roaming"));
        list.opened(&app_data.join("Local"));
        // ホームのドキュメントは、ホームと同じく開いて整える（ホームはこの PC の上にある。ドキュメントがリンクで
        // 別の場所を指すときも、その先の PowerShell の2つを守る。まだない PowerShell のフォルダも、ドキュメントの
        // 先の形で守る）
        for documents in forms(&home.join("Documents")) {
            list.opened(&documents.join("WindowsPowerShell"));
            list.opened(&documents.join("PowerShell"));
        }
    }
    for name in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(path) = absolute_var(get, name) {
            list.opened(&path);
        }
    }
    for (name, default) in [
        ("SystemRoot", r"C:\Windows"),
        ("ProgramFiles", r"C:\Program Files"),
        ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ("ProgramData", r"C:\ProgramData"),
    ] {
        list.opened(Path::new(default));
        if let Some(path) = absolute_var(get, name) {
            list.opened(&path);
        }
    }
    for name in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(path) = absolute_var(get, name) {
            powershell(&mut list, &path.join("Documents"));
        }
    }
    if let Some(os) = os {
        for path in [
            &os.roaming_app_data,
            &os.local_app_data,
            &os.windows,
            &os.program_files,
            &os.program_files_x86,
            &os.program_data,
        ]
        .into_iter()
        .flatten()
        {
            list.opened(path);
        }
        if let Some(documents) = &os.documents {
            powershell(&mut list, documents);
        }
        // ProgramData の OS の値は、プロセスの環境変数 SystemDrive で変わる（K1b）。
        // Windows のフォルダ（環境変数で変わらない）のドライブの ProgramData も守る（設計の決定）
        if let Some(drive) = os.windows.as_deref().and_then(drive_root) {
            list.opened(&drive.join("ProgramData"));
        }
    }
    list
}

/// ドライブ文字のパスなら、そのドライブの根（`X:\`）。それ以外の形（UNC・デバイスの形など）なら None。
#[cfg(windows)]
fn drive_root(path: &Path) -> Option<PathBuf> {
    match path.components().next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(PathBuf::from(format!(r"{}:\", char::from(letter))))
            }
            _ => None,
        },
        _ => None,
    }
}

/// それ自身・祖先・子孫を拒否する場所（mac）。ホーム以外の環境変数に頼らない。
#[cfg(target_os = "macos")]
fn protected_places(
    _get: &dyn Fn(&str) -> Option<OsString>,
    homes: &[PathBuf],
    _os: Option<&KnownPlaces>,
) -> Places {
    let mut list = Places::default();
    for home in homes {
        list.opened(&home.join("Library"));
    }
    for place in [
        "/System",
        "/Library",
        "/Applications",
        "/usr",
        "/bin",
        "/etc",
    ] {
        list.opened(Path::new(place));
    }
    list
}

/// それ自身・祖先・子孫を拒否する場所（Linux など）。ホーム以外の環境変数に頼らない。
#[cfg(not(any(windows, target_os = "macos")))]
fn protected_places(
    _get: &dyn Fn(&str) -> Option<OsString>,
    homes: &[PathBuf],
    _os: Option<&KnownPlaces>,
) -> Places {
    let mut list = Places::default();
    for home in homes {
        list.opened(&home.join(".config"));
        list.opened(&home.join(".local"));
    }
    for place in [
        "/usr", "/etc", "/bin", "/sbin", "/lib", "/boot", "/proc", "/sys", "/dev",
    ] {
        list.opened(Path::new(place));
    }
    list
}

/// `.`・`..` を文字だけで解いて、開いて整えた形と同じ形（Windows では `\\?\` 付き）にそろえる。ファイルに触れない。
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(p) => out.push(verbatim_prefix(p.kind(), p.as_os_str())),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

fn verbatim_prefix(kind: Prefix<'_>, original: &OsStr) -> OsString {
    match kind {
        Prefix::Disk(letter) => OsString::from(format!(r"\\?\{}:", char::from(letter))),
        Prefix::UNC(server, share) => {
            let mut s = OsString::from(r"\\?\UNC\");
            s.push(server);
            s.push(r"\");
            s.push(share);
            s
        }
        _ => original.to_owned(),
    }
}

/// 保管庫にしてよい場所かを判定する。ファイルを読まない純粋な関数。
///
/// `canon` は canonicalize したパス。次のどれかに当たれば `INVALID_ROOT`：
/// ファイルシステムの根・ドライブの根・UNC（ネットワーク）、ホーム（どのホームも）とその祖先、
/// 設定やシステムの場所のそれ自身・祖先・子孫、ホームの直下の `.` から始まるフォルダとその子孫。
/// 比べ方は要素ごとで、Windows では大文字小文字を区別しない。
pub fn check_root(canon: &Path, env: &RootEnv) -> Result<()> {
    if !canon.is_absolute() {
        return Err(invalid_root("絶対パスでない"));
    }
    let mut has_name = false;
    for c in canon.components() {
        match c {
            Component::Prefix(p) => match p.kind() {
                Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {}
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => {
                    return Err(invalid_root(
                        "ネットワーク上のフォルダ（UNC・ネットワークドライブ）は保管庫にできない",
                    ));
                }
                _ => {
                    return Err(invalid_root(
                        "この形のパス（デバイスのパスなど）は保管庫にできない",
                    ));
                }
            },
            Component::RootDir => {}
            Component::CurDir | Component::ParentDir => {
                return Err(invalid_root("パスに . や .. の要素がある"));
            }
            Component::Normal(_) => has_name = true,
        }
    }
    if !has_name {
        return Err(invalid_root(
            "ファイルシステムの根やドライブの根は保管庫にできない",
        ));
    }

    let target = keys(canon);
    let homes: Vec<Vec<String>> = env.homes.iter().map(|home| keys(home)).collect();
    if homes.iter().any(|home| starts_with(home, &target)) {
        return Err(invalid_root(
            "ホームのフォルダと、その上のフォルダは保管庫にできない",
        ));
    }
    for place in &env.protected {
        let place = keys(place);
        if starts_with(&target, &place) || starts_with(&place, &target) {
            return Err(invalid_root(
                "設定・アプリのデータ・システムの場所と、その上や下のフォルダは保管庫にできない",
            ));
        }
    }
    if homes.iter().any(|home| {
        target.len() > home.len()
            && starts_with(&target, home)
            && target[home.len()].starts_with('.')
    }) {
        return Err(invalid_root(
            "ホームの直下の . から始まるフォルダと、その下のフォルダは保管庫にできない",
        ));
    }
    Ok(())
}

/// 比べるための、要素ごとの文字列。`C:` と `\\?\C:` を同じに扱い、Windows では大文字にそろえる。
fn keys(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| {
            let s = match c {
                Component::Prefix(p) => match p.kind() {
                    Prefix::Disk(l) | Prefix::VerbatimDisk(l) => format!("{}:", char::from(l)),
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
                        r"\\{}\{}",
                        server.to_string_lossy(),
                        share.to_string_lossy()
                    ),
                    _ => p.as_os_str().to_string_lossy().into_owned(),
                },
                Component::RootDir => "/".to_owned(),
                Component::CurDir => ".".to_owned(),
                Component::ParentDir => "..".to_owned(),
                Component::Normal(s) => s.to_string_lossy().into_owned(),
            };
            if cfg!(windows) { s.to_uppercase() } else { s }
        })
        .collect()
}

fn starts_with(path: &[String], base: &[String]) -> bool {
    path.len() >= base.len() && path[..base.len()] == *base
}

/// 入力のパスの文字列に `.`・`..` の要素があるか。ファイルを読まない純粋な関数。
///
/// 区切りは、Windows の形では `\` と `/`、ほかは `/`。Windows の形では、パスを整えるときに
/// 要素の末尾の `.` と空白が落とされるので、`.` と空白だけでできた要素（`...`・`.. ` など）も含める。
pub(crate) fn has_dot_segment(input: &str, windows_style: bool) -> bool {
    segments(input, windows_style).any(|seg| {
        if windows_style {
            seg.contains('.') && seg.chars().all(|c| c == '.' || c == ' ')
        } else {
            seg == "." || seg == ".."
        }
    })
}

fn segments(input: &str, windows_style: bool) -> impl Iterator<Item = &str> {
    input.split(move |c| c == '/' || (windows_style && c == '\\'))
}

/// init と open の入力のパスの、要素ごとの問題。ファイルを読まない純粋な関数。
///
/// - Windows の形では、UNC（`\\server\…`・`//server/…`）とデバイスの形（`\\?\`・`\\.\`・`\??\`、
///   `\\?\GLOBALROOT` など）の接頭辞（canonicalize の前に断り、ネットワークや装置に触れない）
/// - `.`・`..` の要素（`has_dot_segment`）
/// - Windows の形では、末尾が `.` か空白の要素（Windows はパスを整えるときに落とし、別の場所として扱うため）
/// - Windows の予約名（relpath と同じ判定。拡張子が付いていても）
pub(crate) fn input_path_problem(input: &str, windows_style: bool) -> Option<&'static str> {
    if windows_style && has_unc_or_device_prefix(input) {
        return Some(
            "ネットワークの場所（UNC）やデバイスの形（先頭に区切りが2つ続くもの）のパスは使えない（ドライブ文字から始まる絶対パスで書く）",
        );
    }
    if has_dot_segment(input, windows_style) {
        return Some("パスに . や .. の要素がある（. や .. を使わない絶対パスで書く）");
    }
    for seg in segments(input, windows_style) {
        if windows_style && (seg.ends_with('.') || seg.ends_with(' ')) {
            return Some("パスに、末尾が . か空白の要素がある");
        }
        if is_windows_reserved_name(seg) {
            return Some("パスに、Windows の予約名（CON・NUL・COM1 など）の要素がある");
        }
    }
    None
}

/// UNC（`\\server\…`・`//server/…`）か、デバイスの形（`\\?\`・`\\.\`・`\??\`）の接頭辞か。文字列だけで判定する。
fn has_unc_or_device_prefix(input: &str) -> bool {
    let mut chars = input.chars();
    let separator = |c: Option<char>| matches!(c, Some('\\' | '/'));
    (separator(chars.next()) && separator(chars.next()))
        || input.starts_with("\\??\\")
        || input.starts_with("/??/")
}

/// init と open の入力のパスを確かめる：要素ごとの問題がないこと（`input_path_problem`）、絶対パスであること。
pub(crate) fn check_input_path(path: &Path) -> Result<()> {
    if let Some(problem) = input_path_problem(&path.to_string_lossy(), cfg!(windows)) {
        return Err(invalid_root(problem));
    }
    if !path.is_absolute() {
        return Err(invalid_root("絶対パスでない"));
    }
    Ok(())
}

/// 入れ子の保管庫の判定：祖先に `.vault/vault.json` があれば `INVALID_ROOT`。子孫は調べない。
pub(crate) fn check_no_ancestor_vault(canon: &Path) -> Result<()> {
    for ancestor in canon.ancestors().skip(1) {
        let marker = ancestor.join(RESERVED_NAME).join(VAULT_JSON);
        match fs::symlink_metadata(&marker) {
            Ok(_) => {
                return Err(invalid_root(
                    "上のフォルダがすでに保管庫になっている（保管庫の中に保管庫は作れない）",
                ));
            }
            Err(e) if is_not_found(&e) => {}
            Err(e) => {
                return Err(VaultError::io(
                    "上のフォルダが保管庫かどうかを調べられない",
                    &e,
                ));
            }
        }
    }
    Ok(())
}

/// 「ない」とみなすエラーか（途中がファイルのときも含める）。
pub(crate) fn is_not_found(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn invalid_root(message: &str) -> VaultError {
    VaultError::new(ErrorCode::InvalidRoot, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// このプラットフォームの根からの絶対パス（Windows は `\\?\C:\`、ほかは `/`）。ファイルには触れない。
    fn abs(parts: &[&str]) -> PathBuf {
        let mut p = if cfg!(windows) {
            PathBuf::from(r"\\?\C:\")
        } else {
            PathBuf::from("/")
        };
        for part in parts {
            p.push(part);
        }
        p
    }

    /// 偽の RootEnv。Windows・mac・Linux の場所を、このプラットフォームの形でまとめて入れる。
    fn fake_env() -> RootEnv {
        let home = abs(&["Profiles", "u"]);
        let protected = vec![
            home.join("AppData"),
            home.join("AppData").join("Roaming"),
            home.join("AppData").join("Local"),
            home.join("Library"),
            home.join(".config"),
            home.join(".local"),
            abs(&["Windows"]),
            abs(&["Program Files"]),
            abs(&["Program Files (x86)"]),
            abs(&["ProgramData"]),
            abs(&["System"]),
            abs(&["Library"]),
            abs(&["Applications"]),
            abs(&["usr"]),
            abs(&["etc"]),
            abs(&["bin"]),
            abs(&["proc"]),
            home.join("Documents").join("WindowsPowerShell"),
            home.join("Documents").join("PowerShell"),
        ];
        RootEnv {
            homes: vec![home],
            protected,
        }
    }

    fn rejected(p: &Path, env: &RootEnv) -> bool {
        match check_root(p, env) {
            Ok(()) => false,
            Err(e) => {
                assert_eq!(e.code(), ErrorCode::InvalidRoot, "{}", p.display());
                true
            }
        }
    }

    #[test]
    fn check_root_table_rejected() {
        let env = fake_env();
        let home = abs(&["Profiles", "u"]);
        let appdata = home.join("AppData");
        let cases: Vec<PathBuf> = vec![
            // ファイルシステムの根・ドライブの根
            abs(&[]),
            // ホームと、その祖先
            home.clone(),
            abs(&["Profiles"]),
            // 設定とアプリのデータ（それ自身・祖先・子孫）
            appdata.clone(),
            appdata.join("LocalLow").join("x"),
            appdata.join("Roaming"),
            appdata
                .join("Roaming")
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Startup"),
            appdata.join("Local"),
            appdata.join("Local").join("Temp"),
            home.join("Library"),
            home.join("Library").join("LaunchAgents"),
            home.join(".config"),
            home.join(".config").join("autostart"),
            home.join(".config").join("systemd").join("user"),
            home.join(".local"),
            home.join(".local").join("share").join("x"),
            // ホームの直下の . から始まるフォルダと、その子孫
            home.join(".ssh"),
            home.join(".gnupg").join("x"),
            home.join(".cache"),
            // PowerShell のプロファイルのフォルダ（それ自身・祖先・子孫）
            home.join("Documents"),
            home.join("Documents").join("WindowsPowerShell"),
            home.join("Documents").join("PowerShell").join("x"),
            // システムのフォルダ（それ自身・子孫。祖先は上の根）
            abs(&["Windows"]),
            abs(&["Windows", "System32", "x"]),
            abs(&["Program Files"]),
            abs(&["Program Files", "App", "v"]),
            abs(&["Program Files (x86)", "v"]),
            abs(&["ProgramData", "v"]),
            abs(&["System", "v"]),
            abs(&["Library", "v"]),
            abs(&["Applications", "v"]),
            abs(&["usr", "local", "v"]),
            abs(&["etc", "v"]),
            abs(&["bin"]),
            abs(&["proc", "1"]),
            // . や .. を含むもの、絶対パスでないもの
            // （\\?\ の付いたパスに push("..") すると上の要素が消えるので、文字列で作る）
            dotted(),
            PathBuf::from("v"),
            PathBuf::from("data").join("v"),
        ];
        for p in &cases {
            assert!(rejected(p, &env), "拒否されるべき：{}", p.display());
        }
    }

    #[test]
    fn check_root_table_accepted() {
        let env = fake_env();
        let home = abs(&["Profiles", "u"]);
        let cases = [
            home.join("Vault"),
            home.join("Documents").join("MyVault"),
            home.join("a.b"),
            home.join("x").join(".config"), // ホームの直下でない . のフォルダ
            home.join("AppDataX"),          // AppData と名前が似ているだけ
            abs(&["data", "v"]),
            abs(&["Profiles", "other"]),
            abs(&["usrx"]),
        ];
        for p in &cases {
            assert!(check_root(p, &env).is_ok(), "通るべき：{}", p.display());
        }
    }

    #[cfg(windows)]
    #[test]
    fn check_root_windows_specific() {
        let env = fake_env();
        for bad in [
            r"C:\",
            r"\\?\D:\",
            r"\\server\share\v",
            r"\\?\UNC\server\share\v",
            r"\\.\C:\v",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\v",
            // RootEnv と大文字小文字だけが違うパス
            r"\\?\c:\PROFILES\U\appdata\roaming\x",
            r"\\?\C:\profiles\u\APPDATA\LocalLow\x",
            r"\\?\c:\windows\system32",
            r"\\?\C:\Profiles\U",
            // \\?\ の付かない形でも同じに扱う
            r"C:\Profiles\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
            r"C:\Profiles\u\.SSH",
            r"C:x",
            r"\Users\u\v",
        ] {
            assert!(rejected(Path::new(bad), &env), "拒否されるべき：{bad}");
        }
        for ok in [r"\\?\D:\data\v", r"D:\data\v", r"\\?\C:\Profiles\u\Vault"] {
            assert!(check_root(Path::new(ok), &env).is_ok(), "通るべき：{ok}");
        }
    }

    #[test]
    fn error_messages_do_not_contain_the_path() {
        let env = fake_env();
        let p = abs(&["Profiles", "u", "AppData", "secret-folder"]);
        let err = check_root(&p, &env).unwrap_err();
        assert!(!err.message().contains("secret-folder"));
        assert!(!err.message().contains("Profiles"));
    }

    /// 入力のパスの . と ..（ファイルを読まない純粋な関数の表のテスト）。
    #[test]
    fn dot_segments_table() {
        let windows_bad = [
            r"C:\Profiles\u\new\..\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\v",
            r"D:\a\.\v",
            r"D:\a\..\v",
            "/home/u/new/../.config/autostart/v",
            "D:/a/./v",
            r"\\?\C:\a\..\v",
            r"D:\a\...\v",
            r"D:\a\.. \v",
            r"D:\a\. \v",
            r"D:\a\..",
        ];
        for s in windows_bad {
            assert!(
                has_dot_segment(s, true),
                "Windows の形で拒否されるべき：{s}"
            );
        }
        let unix_bad = [
            "/home/u/new/../.config/autostart/v",
            "/home/u/./v",
            "/home/u/..",
        ];
        for s in unix_bad {
            assert!(has_dot_segment(s, false), "拒否されるべき：{s}");
        }
        let ok = [
            r"D:\a\.b\v",
            r"D:\a\..b\v",
            r"D:\a\b.\v",
            r"D:\a\.config\v",
            "/home/u/..x/v",
            "/home/u/.ssh-like/v",
        ];
        for s in ok {
            assert!(!has_dot_segment(s, true), "通るべき：{s}");
            assert!(!has_dot_segment(s, false), "通るべき：{s}");
        }
        // Unix の形では \ は区切りでない
        assert!(!has_dot_segment(r"D:\a\..\v", false));
    }

    /// `..` の要素を含む絶対パス。
    fn dotted() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\data\..\v")
        } else {
            PathBuf::from("/data/../v")
        }
    }

    /// 入力のパスの、末尾が . か空白の要素（Windows の形）と、Windows の予約名の要素。
    #[test]
    fn input_path_problem_table() {
        let windows_bad = [
            r"D:\a\v.",
            r"D:\a\v ",
            r"D:\a\x.\v",
            r"D:\a\x \v",
            r"D:\a\x..\v",
            "D:/a/v.",
            r"\\?\D:\a\v.",
            // UNC とデバイスの形（文字列の段階で断る。ネットワークや装置に触れない）
            r"\\?\D:\a\v",
            r"\\?\UNC\server\share\v",
            r"\\server\share\v",
            "//server/share/v",
            r"\/server\share",
            r"\\.\C:\v",
            r"\\.\PhysicalDrive0",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\v",
            r"\??\C:\v",
            r"D:\CON\v",
            r"D:\a\nul.txt",
            r"D:\a\COM1",
            r"D:\a\lpt¹\v",
            r"D:\a\CON .txt\v",
            r"D:\a\clock$",
        ];
        for s in windows_bad {
            assert!(
                input_path_problem(s, true).is_some(),
                "Windows の形で拒否されるべき：{s}"
            );
        }
        let windows_ok = [
            r"D:\a\v",
            "D:/a/v",
            r"D:\a\.v",
            r"D:\a\v.x",
            r"D:\a\CONSOLE",
            r"D:\a\COM10",
            r"D:\a\v x",
        ];
        for s in windows_ok {
            assert_eq!(input_path_problem(s, true), None, "通るべき：{s}");
        }
        // Unix の形：予約名は拒否する（relpath と同じ判定）。末尾の . と空白は Unix では意味がないので通す
        assert!(input_path_problem("/a/con/v", false).is_some());
        assert!(input_path_problem("/a/NUL.txt", false).is_some());
        assert_eq!(input_path_problem("/a/v.", false), None);
        assert_eq!(input_path_problem("/a/v ", false), None);
        // . と .. は、どちらの形でも
        assert!(input_path_problem("/a/../v", false).is_some());
        assert!(input_path_problem(r"D:\a\..\v", true).is_some());
    }

    #[test]
    fn check_input_path_rejects_dots_and_relative_paths() {
        let bad = [
            dotted(),
            PathBuf::from(format!("{}{}", abs(&["a"]).display(), "/./v")),
            PathBuf::from("v"),
            PathBuf::from("a").join("b"),
        ];
        for p in &bad {
            let err = check_input_path(p).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRoot, "{}", p.display());
        }
        let plain = if cfg!(windows) {
            PathBuf::from(r"C:\a\v")
        } else {
            PathBuf::from("/a/v")
        };
        assert!(check_input_path(&plain).is_ok());
        // Windows：\\?\ の付いた形（デバイスの形）は、入力では断る（canonicalize の前）
        if cfg!(windows) {
            let err = check_input_path(&abs(&["a", "v"])).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRoot);
        } else {
            assert!(check_input_path(&abs(&["a", "v"])).is_ok());
        }
    }

    #[cfg(windows)]
    mod from_lookup_windows {
        use super::*;
        use std::collections::HashMap;

        fn env_with(vars: &[(&str, &str)]) -> Result<RootEnv> {
            let map: HashMap<String, OsString> = vars
                .iter()
                .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
                .collect();
            RootEnv::from_lookup(&|name| map.get(name).cloned())
        }

        fn contains(env: &RootEnv, p: &str) -> bool {
            let want = keys(&lexical(Path::new(p)));
            env.protected.iter().any(|q| keys(q) == want)
        }

        const HOME: &str = r"C:\vault-core-fake-home\u";

        #[test]
        fn home_unknown_is_rejected() {
            for home in [
                None,
                Some(""),
                Some(r"Users\u"),
                Some(r"\Users\u"),
                Some(r"C:u"),
            ] {
                let vars: Vec<(&str, &str)> =
                    home.map(|h| ("USERPROFILE", h)).into_iter().collect();
                let err = env_with(&vars).unwrap_err();
                assert_eq!(err.code(), ErrorCode::InvalidRoot, "{home:?}");
            }
        }

        #[test]
        fn empty_or_relative_values_use_the_defaults() {
            for bad in ["", "Windows", r".\x", r"\x", "C:x"] {
                let env = env_with(&[
                    ("USERPROFILE", HOME),
                    ("APPDATA", bad),
                    ("LOCALAPPDATA", bad),
                    ("SystemRoot", bad),
                    ("ProgramFiles", bad),
                    ("ProgramFiles(x86)", bad),
                    ("ProgramData", bad),
                ])
                .unwrap();
                for default in [
                    r"C:\vault-core-fake-home\u\AppData",
                    r"C:\vault-core-fake-home\u\AppData\Roaming",
                    r"C:\vault-core-fake-home\u\AppData\Local",
                    r"C:\Windows",
                    r"C:\Program Files",
                    r"C:\Program Files (x86)",
                    r"C:\ProgramData",
                ] {
                    assert!(contains(&env, default), "{bad:?} → 既定値 {default}");
                }
            }
            // 変数がないときも同じ
            let env = env_with(&[("USERPROFILE", HOME)]).unwrap();
            assert!(contains(&env, r"C:\Windows"));
            assert!(contains(&env, r"C:\vault-core-fake-home\u\AppData\Roaming"));
        }

        /// 環境変数の値と既定値の、両方がいつも入る（差し替えられても既定の場所は守る）。
        #[test]
        fn both_the_values_and_the_defaults_are_protected() {
            let env = env_with(&[
                ("USERPROFILE", HOME),
                ("APPDATA", r"D:\fake-roaming"),
                ("LOCALAPPDATA", r"D:\fake-local"),
                ("SystemRoot", r"E:\fake-windows"),
                ("ProgramFiles", r"E:\fake-pf"),
                ("ProgramFiles(x86)", r"E:\fake-pf86"),
                ("ProgramData", r"E:\fake-pd"),
            ])
            .unwrap();
            for place in [
                r"D:\fake-roaming",
                r"D:\fake-local",
                r"E:\fake-windows",
                r"E:\fake-pf",
                r"E:\fake-pf86",
                r"E:\fake-pd",
                r"C:\vault-core-fake-home\u\AppData",
                r"C:\vault-core-fake-home\u\AppData\Roaming",
                r"C:\vault-core-fake-home\u\AppData\Local",
                r"C:\Windows",
                r"C:\Program Files",
                r"C:\Program Files (x86)",
                r"C:\ProgramData",
            ] {
                assert!(contains(&env, place), "{place}");
            }
            for bad in [
                r"\\?\E:\fake-windows\System32",
                r"\\?\C:\Windows\System32",
                r"\\?\D:\fake-roaming\Microsoft\Windows\Start Menu\Programs\Startup",
                r"\\?\C:\vault-core-fake-home\u\AppData\Roaming\x",
            ] {
                assert!(rejected(Path::new(bad), &env), "{bad}");
            }
        }

        /// PowerShell のプロファイルのフォルダ（それ自身・祖先・子孫）。
        #[test]
        fn powershell_profile_folders_are_protected() {
            let env = env_with(&[("USERPROFILE", HOME)]).unwrap();
            let docs = r"\\?\C:\vault-core-fake-home\u\Documents";
            for bad in [
                docs.to_owned(),
                format!(r"{docs}\WindowsPowerShell"),
                format!(r"{docs}\WindowsPowerShell\x"),
                format!(r"{docs}\PowerShell"),
                format!(r"{docs}\PowerShell\x"),
            ] {
                assert!(rejected(Path::new(&bad), &env), "{bad}");
            }
            for ok in [format!(r"{docs}\MyVault"), format!(r"{docs}\Work\v")] {
                assert!(check_root(Path::new(&ok), &env).is_ok(), "{ok}");
            }
        }

        /// OneDrive などで移した「ドキュメント」の下の PowerShell のプロファイルも守る。
        #[test]
        fn powershell_profile_folders_under_onedrive_are_protected() {
            let env = env_with(&[
                ("USERPROFILE", HOME),
                ("OneDrive", r"D:\fake-onedrive"),
                ("OneDriveConsumer", r"D:\fake-onedrive-consumer"),
                ("OneDriveCommercial", r"D:\fake-onedrive-work"),
            ])
            .unwrap();
            for root in [
                r"\\?\D:\fake-onedrive",
                r"\\?\D:\fake-onedrive-consumer",
                r"\\?\D:\fake-onedrive-work",
            ] {
                for bad in [
                    format!(r"{root}\Documents"),
                    format!(r"{root}\Documents\WindowsPowerShell"),
                    format!(r"{root}\Documents\PowerShell\x"),
                ] {
                    assert!(rejected(Path::new(&bad), &env), "{bad}");
                }
                for ok in [
                    format!(r"{root}\Documents\MyVault"),
                    format!(r"{root}\Other"),
                ] {
                    assert!(check_root(Path::new(&ok), &env).is_ok(), "{ok}");
                }
            }
            // 絶対パスでない値は使わない（ホームの Documents だけが入る）
            let env = env_with(&[("USERPROFILE", HOME), ("OneDrive", r"fake-onedrive")]).unwrap();
            assert!(!contains(&env, r"fake-onedrive\Documents\PowerShell"));
            assert!(contains(
                &env,
                r"C:\vault-core-fake-home\u\Documents\PowerShell"
            ));
        }

        #[test]
        fn values_are_normalized_to_the_verbatim_form() {
            let env = env_with(&[("USERPROFILE", r"C:\vault-core-fake-home\x\..\u")]).unwrap();
            assert_eq!(env.homes, [PathBuf::from(r"\\?\C:\vault-core-fake-home\u")]);
            // 実在するフォルダは、開いて整えた形（canonicalize と同じ場所）になる。文字だけの形と同じ場所なので1つ
            let env = env_with(&[("USERPROFILE", env!("CARGO_MANIFEST_DIR"))]).unwrap();
            let canonical = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
            assert_eq!(env.homes.len(), 1, "{:?}", env.homes);
            assert_eq!(env.homes[0], canonical);
            assert!(env.homes[0].to_string_lossy().starts_with(r"\\?\"));
        }

        #[test]
        fn env_from_lookup_protects_the_startup_folder() {
            let env = env_with(&[("USERPROFILE", HOME)]).unwrap();
            for bad in [
                r"\\?\C:\vault-core-fake-home\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
                r"\\?\C:\vault-core-fake-home\u\AppData\LocalLow\x",
                r"\\?\C:\vault-core-fake-home\u\AppData\Local\Temp",
                r"\\?\C:\vault-core-fake-home\u\.ssh",
                r"\\?\C:\vault-core-fake-home\u",
                r"\\?\C:\Windows\x",
            ] {
                assert!(rejected(Path::new(bad), &env), "{bad}");
            }
            assert!(check_root(Path::new(r"\\?\C:\vault-core-fake-home\u\Vault"), &env).is_ok());
        }

        // -------------------------------------------------------------------
        // OS の値（U1〜U5）。ドライブの文字は、この PC にないもの（R:・S:）を使う
        // （開けないので、文字だけの形が入る）。
        // -------------------------------------------------------------------

        /// 環境変数と OS の値の、両方を偽にして作る。
        fn env_with_os(vars: &[(&str, &str)], os: KnownPlaces) -> Result<RootEnv> {
            let map: HashMap<String, OsString> = vars
                .iter()
                .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
                .collect();
            RootEnv::from_sources(
                &|name| map.get(name).cloned(),
                OsPlaces::Asked(Box::new(os)),
            )
        }

        /// OS のホームだけが取れた値。
        fn os_home(path: &str) -> KnownPlaces {
            KnownPlaces {
                profile: Some(PathBuf::from(path)),
                ..KnownPlaces::default()
            }
        }

        /// U1：OS のホームが環境変数のホームと違うときは、両方を守る（ホームとその祖先・AppData・ホームの直下の
        /// `.`・ドキュメントの下の PowerShell）。ホームの下のほかのフォルダは通す。
        #[test]
        fn u1_both_the_os_home_and_the_variable_home_are_protected() {
            let env = env_with_os(
                &[("USERPROFILE", r"R:\fake-variable-home\u")],
                os_home(HOME),
            )
            .unwrap();
            for home in [
                r"\\?\R:\fake-variable-home\u",
                r"\\?\C:\vault-core-fake-home\u",
            ] {
                for bad in [
                    home.to_owned(),
                    format!(r"{home}\AppData\Roaming\x"),
                    format!(r"{home}\AppData\Local\Temp"),
                    format!(r"{home}\.ssh"),
                    format!(r"{home}\Documents\PowerShell"),
                ] {
                    assert!(rejected(Path::new(&bad), &env), "{bad}");
                }
                let ok = format!(r"{home}\Vault");
                assert!(check_root(Path::new(&ok), &env).is_ok(), "{ok}");
            }
            for ancestor in [r"\\?\R:\fake-variable-home", r"\\?\C:\vault-core-fake-home"] {
                assert!(rejected(Path::new(ancestor), &env), "{ancestor}");
            }
        }

        /// U2：OS にホームを問い合わせられないときは INVALID_ROOT（新しい文）。環境変数のホームがないときは、OS の
        /// ホームが取れても今までどおり INVALID_ROOT（設計の決定）。
        #[test]
        fn u2_the_home_must_be_known_to_both() {
            let err = env_with_os(&[("USERPROFILE", HOME)], KnownPlaces::default()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRoot);
            assert!(err.message().contains("OS にホーム"), "{}", err.message());
            for vars in [
                &[][..],
                &[("USERPROFILE", "")][..],
                &[("USERPROFILE", r"Users\u")][..],
            ] {
                let err = env_with_os(vars, os_home(HOME)).unwrap_err();
                assert_eq!(err.code(), ErrorCode::InvalidRoot, "{vars:?}");
                assert!(err.message().contains("環境変数"), "{}", err.message());
            }
        }

        /// U3：ほかの OS の値が取れなくても、既定の値と環境変数の値は入る。
        #[test]
        fn u3_missing_os_values_keep_the_defaults_and_the_variables() {
            let env = env_with_os(
                &[("USERPROFILE", HOME), ("APPDATA", r"R:\var-roaming")],
                os_home(HOME),
            )
            .unwrap();
            for place in [
                r"C:\Windows",
                r"C:\Program Files",
                r"C:\Program Files (x86)",
                r"C:\ProgramData",
                r"C:\vault-core-fake-home\u\AppData",
                r"C:\vault-core-fake-home\u\AppData\Roaming",
                r"C:\vault-core-fake-home\u\AppData\Local",
                r"C:\vault-core-fake-home\u\Documents\PowerShell",
                r"R:\var-roaming",
            ] {
                assert!(contains(&env, place), "{place}");
            }
        }

        /// OS の値は、どれも足す（環境変数の値と既定の値は外さない）。
        #[test]
        fn os_values_are_added() {
            let os = KnownPlaces {
                roaming_app_data: Some(PathBuf::from(r"R:\os-roaming")),
                local_app_data: Some(PathBuf::from(r"R:\os-local")),
                documents: Some(PathBuf::from(r"R:\os-docs")),
                windows: Some(PathBuf::from(r"R:\os-windows")),
                program_files: Some(PathBuf::from(r"R:\os-pf")),
                program_files_x86: Some(PathBuf::from(r"R:\os-pf86")),
                program_data: Some(PathBuf::from(r"R:\os-pd")),
                ..os_home(HOME)
            };
            let env = env_with_os(
                &[("USERPROFILE", HOME), ("SystemRoot", r"S:\var-windows")],
                os,
            )
            .unwrap();
            for place in [
                r"R:\os-roaming",
                r"R:\os-local",
                r"R:\os-docs\WindowsPowerShell",
                r"R:\os-docs\PowerShell",
                r"R:\os-windows",
                r"R:\os-pf",
                r"R:\os-pf86",
                r"R:\os-pd",
                r"S:\var-windows",
                r"C:\Windows",
            ] {
                assert!(contains(&env, place), "{place}");
            }
            for bad in [
                r"\\?\R:\os-roaming\Microsoft\Windows\Start Menu\Programs\Startup",
                r"\\?\R:\os-docs",
                r"\\?\R:\os-windows\System32",
            ] {
                assert!(rejected(Path::new(bad), &env), "{bad}");
            }
            assert!(check_root(Path::new(r"\\?\R:\os-docs\MyVault"), &env).is_ok());
        }

        /// U4：ドキュメント（OS の値）が別のドライブなら、その下の PowerShell の2つと、ドキュメント自身（その祖先）を
        /// 断り、ドキュメントの下のほかのフォルダは通す。ドキュメントから作る値は文字だけの形（開かない）。
        #[test]
        fn u4_documents_on_another_drive() {
            let os = KnownPlaces {
                documents: Some(PathBuf::from(r"S:\fake-docs")),
                ..os_home(HOME)
            };
            let env = env_with_os(&[("USERPROFILE", HOME)], os).unwrap();
            for bad in [
                r"\\?\S:\fake-docs",
                r"\\?\S:\fake-docs\WindowsPowerShell",
                r"\\?\S:\fake-docs\PowerShell\x",
            ] {
                assert!(rejected(Path::new(bad), &env), "{bad}");
            }
            assert!(check_root(Path::new(r"\\?\S:\fake-docs\MyVault"), &env).is_ok());
            let docs: Vec<&PathBuf> = env
                .protected
                .iter()
                .filter(|p| keys(p).first().is_some_and(|d| d == "S:"))
                .collect();
            assert_eq!(
                docs,
                [
                    &PathBuf::from(r"\\?\S:\fake-docs\WindowsPowerShell"),
                    &PathBuf::from(r"\\?\S:\fake-docs\PowerShell")
                ],
                "文字だけの形だけが入る"
            );
        }

        /// U5：OS の値が UNC なら、文字だけで整える（ネットワークに触れない）。Windows のフォルダが UNC なら、
        /// そのドライブの ProgramData は足さない（ドライブ文字のパスだけ）。
        #[test]
        fn u5_unc_values_are_kept_as_text() {
            let os = KnownPlaces {
                roaming_app_data: Some(PathBuf::from(r"\\vault-core-test.invalid\share\Roaming")),
                documents: Some(PathBuf::from(r"\\vault-core-test.invalid\share\Docs")),
                windows: Some(PathBuf::from(r"\\vault-core-test.invalid\share\Windows")),
                ..os_home(HOME)
            };
            let env = env_with_os(&[("USERPROFILE", HOME)], os).unwrap();
            for place in [
                r"\\vault-core-test.invalid\share\Roaming",
                r"\\vault-core-test.invalid\share\Docs\PowerShell",
                r"\\vault-core-test.invalid\share\Windows",
            ] {
                assert!(contains(&env, place), "{place}");
            }
            assert!(
                env.protected
                    .iter()
                    .any(|p| p.as_os_str() == r"\\?\UNC\vault-core-test.invalid\share\Roaming"),
                "文字だけの形"
            );
            let program_data: Vec<Vec<String>> = env
                .protected
                .iter()
                .map(|p| keys(p))
                .filter(|k| k.last().is_some_and(|n| n == "PROGRAMDATA"))
                .collect();
            assert_eq!(
                program_data,
                [keys(Path::new(r"C:\ProgramData"))],
                "UNC の Windows のフォルダからは足さない"
            );
        }

        /// Windows のフォルダ（OS の値）のドライブの ProgramData も守る（OS の ProgramData が取れないときも。
        /// 設計の決定）。`\\?\` の付いた形からも取る。
        #[test]
        fn program_data_on_the_windows_drive_is_protected() {
            for windows in [r"S:\fake-windows", r"\\?\S:\fake-windows"] {
                let os = KnownPlaces {
                    windows: Some(PathBuf::from(windows)),
                    program_data: None,
                    ..os_home(HOME)
                };
                let env = env_with_os(&[("USERPROFILE", HOME), ("ProgramData", r"R:\var-pd")], os)
                    .unwrap();
                assert!(contains(&env, r"S:\ProgramData"), "{windows}");
                for bad in [
                    r"\\?\S:\ProgramData",
                    r"\\?\S:\ProgramData\x",
                    r"\\?\R:\var-pd\x",
                    r"\\?\C:\ProgramData\x",
                ] {
                    assert!(rejected(Path::new(bad), &env), "{windows}：{bad}");
                }
            }
        }

        // -------------------------------------------------------------------
        // 開いて整える形（Places::opened）と、文字だけの形。本物のフォルダと
        // ジャンクションを、リポジトリの target の下にだけ作る（Drop で、ジャンクションを外してから消す）。
        // -------------------------------------------------------------------

        /// テスト用のフォルダ（リポジトリの target\vault-core-unit-tests の下だけ）。
        struct LinkDir {
            path: PathBuf,
            links: Vec<PathBuf>,
        }

        impl LinkDir {
            fn base() -> PathBuf {
                lexical(
                    &Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("..")
                        .join("..")
                        .join("target")
                        .join("vault-core-unit-tests"),
                )
            }

            fn new(name: &str) -> Self {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = Self::base().join(format!("{name}-{}-{nanos}", std::process::id()));
                fs::create_dir_all(&path).unwrap();
                Self {
                    path: fs::canonicalize(&path).unwrap(),
                    links: Vec::new(),
                }
            }

            /// フォルダを作る（`\\?\` の付いた、開いて整えた形で返す）。
            fn dir(&self, rel: &str) -> PathBuf {
                let path = self.path.join(rel);
                fs::create_dir_all(&path).unwrap();
                path
            }

            /// `link`（まだないこと）に、`target` を指すジャンクションを作る。
            fn junction(&mut self, rel: &str, target: &Path) -> PathBuf {
                let link = self.path.join(rel);
                let out = std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(plain(&link))
                    .arg(plain(target))
                    .output()
                    .unwrap();
                assert!(out.status.success(), "ジャンクションを作れない");
                self.links.push(link.clone());
                link
            }
        }

        impl Drop for LinkDir {
            fn drop(&mut self) {
                // ジャンクションだけを外す（指す先は消さない）
                for link in self.links.iter().rev() {
                    let _ = fs::remove_dir(link);
                }
                // 基を開いて整えられないときは、消さない（どこを消すかを確かめられないため）
                if let Ok(base) = fs::canonicalize(Self::base())
                    && self.path.starts_with(&base)
                    && self.path != base
                {
                    let _ = fs::remove_dir_all(&self.path);
                }
            }
        }

        /// `\\?\` を外した形（cmd に渡す・環境変数の値にする）。
        fn plain(path: &Path) -> String {
            path.to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_owned()
        }

        /// 8.3 の短い名前の形（cmd の `%~sI`）。短い名前がない（8.3 が無効なドライブ）なら None。
        fn short_name(path: &Path) -> Option<String> {
            use std::os::windows::process::CommandExt;
            let long = plain(path);
            let out = std::process::Command::new("cmd")
                .raw_arg(format!(r#"/C for %I in ("{long}") do @echo %~sI"#))
                .output()
                .ok()?;
            let short = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            (out.status.success() && !short.is_empty() && !short.eq_ignore_ascii_case(&long))
                .then_some(short)
        }

        /// U6 の確かめ：環境変数のホームを `home` にし（OS のホームは偽）、本物のフォルダ `real`・その下の
        /// AppData\Roaming\x・その下の .ssh を断り、その下の Vault は通す。
        fn assert_real_home_protected(label: &str, home: &str, real: &Path) {
            let env = env_with_os(&[("USERPROFILE", home)], os_home(r"R:\fake-os-home\u")).unwrap();
            for bad in [
                real.to_path_buf(),
                real.join("AppData").join("Roaming").join("x"),
                real.join(".ssh"),
            ] {
                assert!(rejected(&bad, &env), "{label}：{}", bad.display());
            }
            let ok = real.join("Vault");
            assert!(check_root(&ok, &env).is_ok(), "{label}：{}", ok.display());
        }

        /// U6：環境変数のホームが、本物のフォルダを指すジャンクションでも（OS のホームは偽）、指す先・
        /// その下の AppData\Roaming\x・その下の .ssh を断り、その下の Vault は通す（開いて整えた形が入る。
        /// Places::opened を外すと落ちる）。
        #[test]
        fn u6_a_linked_home_protects_the_real_folder() {
            let mut dir = LinkDir::new("u6");
            let real = dir.dir("real-home-folder-with-a-long-name");
            let link = dir.junction("linked-home", &real);
            assert_real_home_protected("ジャンクション", &plain(&link), &real);
        }

        /// U6 の短い名前の形：環境変数のホームが 8.3 の短い名前の形でも、同じく本物のフォルダを守る。
        /// **テストのフォルダを作るドライブ（リポジトリの target のあるドライブ）に 8.3 の名前がないときは、
        /// 何も確かめずに通る**（出力に「確かめていない」と書く）。
        #[test]
        fn u6_short_name_home_is_checked_only_where_8dot3_names_exist() {
            let dir = LinkDir::new("u6-short");
            let real = dir.dir("real-home-folder-with-a-long-name");
            match short_name(&real) {
                Some(short) => assert_real_home_protected("短い名前", &short, &real),
                None => println!(
                    "U6 の短い名前の形：このドライブに 8.3 の短い名前がないので、確かめていない"
                ),
            }
        }

        /// ホームのドキュメントがジャンクションで別の場所を指すとき、その先の PowerShell の2つと、その先自身
        /// （祖先）を断る（再現。PowerShell のフォルダがまだないときも、あるときも）。その先のほかの
        /// フォルダは通す。
        #[test]
        fn home_documents_linked_elsewhere_are_protected() {
            let mut dir = LinkDir::new("home-docs");
            let home = dir.dir("home");
            let real_docs = dir.dir("realdocs");
            dir.junction(r"home\Documents", &real_docs);
            for existing in [false, true] {
                if existing {
                    fs::create_dir(real_docs.join("PowerShell")).unwrap();
                }
                let env = env_with_os(
                    &[("USERPROFILE", &plain(&home))],
                    os_home(r"R:\fake-os-home\u"),
                )
                .unwrap();
                for bad in [
                    real_docs.clone(),
                    real_docs.join("PowerShell"),
                    real_docs.join("WindowsPowerShell").join("x"),
                ] {
                    assert!(rejected(&bad, &env), "{existing}：{}", bad.display());
                }
                let ok = real_docs.join("MyVault");
                assert!(
                    check_root(&ok, &env).is_ok(),
                    "{existing}：{}",
                    ok.display()
                );
            }
        }

        /// OS のドキュメントの値と OneDrive の値から作るもの（その下の PowerShell の2つ）は、文字だけで整える：
        /// どちらもジャンクションで、指す先に PowerShell のフォルダがあっても、並びには指す先の形が入らない
        /// （ネットワークの場所で待たないため）。
        #[test]
        fn os_and_onedrive_documents_are_kept_as_text() {
            let mut dir = LinkDir::new("os-docs");
            let os_real = dir.dir(r"os-docs-real\PowerShell");
            let os_real = os_real.parent().unwrap().to_path_buf();
            let os_link = dir.junction("os-docs-link", &os_real);
            let one_real = dir.dir(r"onedrive-real\Documents\PowerShell");
            let one_real = one_real.parent().unwrap().parent().unwrap().to_path_buf();
            let one_link = dir.junction("onedrive-link", &one_real);
            let os = KnownPlaces {
                documents: Some(os_link.clone()),
                ..os_home(r"R:\fake-os-home\u")
            };
            let env = env_with_os(
                &[
                    ("USERPROFILE", r"R:\fake-variable-home\u"),
                    ("OneDrive", &plain(&one_link)),
                ],
                os,
            )
            .unwrap();
            let has = |p: &Path| env.protected.iter().any(|q| keys(q) == keys(p));
            assert!(has(&os_link.join("PowerShell")));
            assert!(has(&one_link.join("Documents").join("PowerShell")));
            assert!(
                !has(&os_real.join("PowerShell")),
                "OS のドキュメントの指す先の形が入った"
            );
            assert!(
                !has(&one_real.join("Documents").join("PowerShell")),
                "OneDrive の指す先の形が入った"
            );
        }
    }

    #[cfg(unix)]
    mod from_lookup_unix {
        use super::*;

        #[test]
        fn home_unknown_is_rejected() {
            for home in [None, Some(""), Some("home/u"), Some("./u")] {
                let err = RootEnv::from_lookup(&|name| match (name, home) {
                    ("HOME", Some(h)) => Some(OsString::from(h)),
                    _ => None,
                })
                .unwrap_err();
                assert_eq!(err.code(), ErrorCode::InvalidRoot, "{home:?}");
            }
        }

        #[test]
        fn home_is_used() {
            let env = RootEnv::from_lookup(&|name| {
                (name == "HOME").then(|| OsString::from("/vault-core-fake-home/u"))
            })
            .unwrap();
            assert_eq!(env.homes, [PathBuf::from("/vault-core-fake-home/u")]);
            assert!(
                check_root(Path::new("/vault-core-fake-home/u/.config/autostart"), &env).is_err()
            );
            assert!(check_root(Path::new("/vault-core-fake-home/u/Vault"), &env).is_ok());
        }
    }
}
