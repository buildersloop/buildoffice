//! 既定の保管庫：置き場所（アプリではホーム）の直下の決まった名前のフォルダを、なければ作って開き、
//! 保管庫なら開く。それ以外（保管庫でないフォルダ・リンク・ファイルなど）は、作らず・開かずにエラーを返す。
//!
//! - 状態を持たない（消されたら、次に呼んだときにまた作る）。消す処理はない。
//! - 場所の名前（P＝置き場所＋名前）がリンク（ジャンクション・シンボリックリンク）なら、指す先が保管庫でもたどらない。
//! - 確かめてから作る・開くまでのあいだに場所を差し替えられても、別の場所に作らない・別の場所を変えない。
//!   置き場所をハンドルの最終のパスで解いた形＋名前を「期待する根」にし、init は何かを作る前、open は根を決めた
//!   直後に、期待する根と同じか（リンクをたどらずに着くか）を確かめる。開いたあとも確かめ直す。
//! - エラーの文には、パス・ユーザー名・名前を入れない（init の競合・片づけの注に相対の名前が入ることはある）。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::actor::Actor;
use crate::error::{ErrorCode, ErrorDetail, Result, VaultError};
use crate::guard::{self, RootId};
use crate::root::{RootEnv, check_parent_dir, is_not_found, vault_path_in};
use crate::vault::Vault;

/// 既定の保管庫を開いた結果。
#[derive(Debug)]
pub struct HomeVault {
    /// 開いた保管庫（書ける状態）。
    pub vault: Vault,
    /// この呼び出しで作ったか（作ったなら true、すでにあった保管庫を開いたなら false）。
    pub created: bool,
}

/// `parent` の直下の `name` を既定の保管庫として開く（なければ作る）。書ける状態で開く。
///
/// 1. `vault_path_in`（置き場所と名前の決まり。誤りは `INVALID_ROOT`）
/// 2. `check_parent_dir`（置き場所がすでにあるフォルダか。ない・ファイルなら `INVALID_ROOT`。祖先も作らない）
/// 3. 置き場所をハンドルの最終のパスで解き、期待する根（解いた形＋名前）を決める（解けなければ `INVALID_ROOT`）
/// 4. 場所を、リンクをたどらずに調べる：ない → `Vault::init`（`actor`）のあと開く（created:true）。フォルダ → 開く
///    （保管庫でなければ `NOT_A_VAULT`。作らない・変えない）。リンク・ファイルなど → `INVALID_ROOT`。調べられない →
///    `IO_ERROR`。init が「すでに保管庫」（同時に呼ばれて、ほかが先に作った）で失敗したときだけ、開くへ進む
/// 5. 開いたあと、根が期待する根のままかを確かめ直す（違えば閉じて `INVALID_ROOT`。作ったものは消さない）
///
/// init は成功し open が失敗したときは、作った保管庫は残す（消さない。ルール6）。エラーは open のもの。
pub fn open_or_create_in(
    parent: &Path,
    name: &str,
    env: &RootEnv,
    actor: &Actor,
) -> Result<HomeVault> {
    let path = vault_path_in(parent, name)?;
    check_parent_dir(parent)?;
    let expected = ExpectedRoot::of_parent(parent, name)?;
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(invalid_root(
                "既定の保管庫の場所がリンク（ジャンクション・シンボリックリンク）になっているので、たどらない（作らない・開かない）",
            ));
        }
        Ok(meta) if meta.is_dir() => {
            test_hook!(Step::AfterDefaultVaultCheck);
            return open_expected(&path, &expected, env, false);
        }
        Ok(_) => {
            return Err(invalid_root(
                "既定の保管庫の場所に、フォルダでないもの（ファイルなど）がある",
            ));
        }
        Err(e) if is_not_found(&e) => {}
        Err(e) => return Err(VaultError::io("既定の保管庫の場所を調べられない", &e)),
    }
    test_hook!(Step::AfterDefaultVaultCheck);
    let created = match Vault::init_checked(&path, env, actor, Some(&expected)) {
        Ok(()) => true,
        // 同時に呼ばれて、ほかが先に作った：開くへ進む（開くときにも、期待する根を確かめる）
        Err(e) if e.detail() == Some(ErrorDetail::AlreadyAVault) => false,
        Err(e) => return Err(e),
    };
    open_expected(&path, &expected, env, created)
}

/// OS のホーム（既知のフォルダ Profile。環境変数は使わない）の直下の `name` を、既定の保管庫として開く（なければ作る）。
/// 決まりは `open_or_create_in` と同じ。ホームのパスは vault-core の外へ出さない。Windows 以外は `NOT_IMPLEMENTED`。
pub fn open_or_create_in_home(name: &str, env: &RootEnv, actor: &Actor) -> Result<HomeVault> {
    let home = guard::os_home()?;
    open_or_create_in(&home, name, env, actor)
}

/// 期待する根で開き、開いたあとも確かめ直す（違えば閉じて `INVALID_ROOT`）。
fn open_expected(
    path: &Path,
    expected: &ExpectedRoot,
    env: &RootEnv,
    created: bool,
) -> Result<HomeVault> {
    let vault = Vault::open_checked(path, false, env, Some(expected))?;
    // 開いたあとも念のため。違えば、vault を落として閉じる
    expected.check(vault.root())?;
    expected.check_without_links(path, vault.root_id())?;
    Ok(HomeVault { vault, created })
}

/// 期待する根：置き場所を、リンクをたどって開いたハンドルの最終のパスで解いた形と、保管庫の名前。
#[derive(Debug)]
pub(crate) struct ExpectedRoot {
    parent: PathBuf,
    name: OsString,
}

impl ExpectedRoot {
    /// 置き場所をハンドルの最終のパスで解く。解けなければ `INVALID_ROOT`（文にパスを入れない）。
    fn of_parent(parent: &Path, name: &str) -> Result<Self> {
        let parent = guard::resolve_root_path(parent)
            .map_err(|_| invalid_root("既定の保管庫の置き場所のフォルダの場所を確かめられない"))?;
        Ok(Self::new(parent, name))
    }

    fn new(parent: PathBuf, name: &str) -> Self {
        Self {
            parent,
            name: OsString::from(name),
        }
    }

    /// `actual`（ハンドルの最終のパス）が「置き場所を解いた形＋名前」か。ファイルを読まない純粋な関数。
    ///
    /// 要素ごとに比べる：置き場所の部分は同じ要素の並び（どちらもハンドルの最終のパスなので、同じ場所なら同じ文字）、
    /// 最後の要素は ASCII の範囲だけ大文字小文字を問わない（Unicode の大文字小文字は使わない。大文字小文字の違う
    /// 既存のフォルダも、同じ名前として開く）。
    pub(crate) fn matches(&self, actual: &Path) -> bool {
        match (actual.parent(), actual.file_name()) {
            (Some(parent), Some(last)) => {
                parent == self.parent && last.eq_ignore_ascii_case(&self.name)
            }
            _ => false,
        }
    }

    /// `matches` でなければ `INVALID_ROOT`。
    pub(crate) fn check(&self, actual: &Path) -> Result<()> {
        if self.matches(actual) {
            Ok(())
        } else {
            Err(Self::moved())
        }
    }

    /// `path`（入力のパス）を、最後の要素のリンクをたどらずに開いたものが、根（`id`）と同じフォルダか。違えば
    /// `INVALID_ROOT`、調べられなければ `IO_ERROR`。
    pub(crate) fn check_without_links(&self, path: &Path, id: &RootId) -> Result<()> {
        match guard::is_same_dir_without_links(path, id) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Self::moved()),
            Err(e) => Err(VaultError::io("既定の保管庫の場所を調べられない", &e)),
        }
    }

    /// 確かめたあとで場所が変わったときのエラー（文にパス・名前を入れない）。
    pub(crate) fn moved() -> VaultError {
        invalid_root(
            "既定の保管庫の場所が、確かめたあとで変わった（リンクに差し替えられた など）ので、作らない・開かない。もう一度試す",
        )
    }
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

    /// 突き合わせの表：置き場所の部分は同じ要素の並び、最後の要素は ASCII だけ大文字小文字を問わない。
    #[test]
    fn expected_root_table() {
        let expected = ExpectedRoot::new(abs(&["Profiles", "u"]), "Buildoffice");
        for ok in [
            abs(&["Profiles", "u", "Buildoffice"]),
            abs(&["Profiles", "u", "BUILDOFFICE"]),
            abs(&["Profiles", "u", "buildoffice"]),
            abs(&["Profiles", "u", "bUiLdOfFiCe"]),
        ] {
            assert!(expected.matches(&ok), "通るべき：{}", ok.display());
            assert!(expected.check(&ok).is_ok());
        }
        for bad in [
            // 置き場所の部分が違う（大文字小文字だけの違いも。どちらもハンドルの最終のパスなので、同じ場所なら同じ文字）
            abs(&["PROFILES", "u", "Buildoffice"]),
            abs(&["Profiles", "U", "Buildoffice"]),
            abs(&["Profiles", "other", "Buildoffice"]),
            abs(&["Profiles", "u", "x", "Buildoffice"]),
            abs(&["Profiles", "Buildoffice"]),
            abs(&["Buildoffice"]),
            // 名前が違う
            abs(&["Profiles", "u", "Buildoffice2"]),
            abs(&["Profiles", "u", "Buildoffic"]),
            abs(&["Profiles", "u", "Buildoffice", "x"]),
            abs(&["Profiles", "u"]),
            abs(&[]),
            // ASCII でない文字の大文字小文字・似た字（キリルの е、全角）は同じにしない
            abs(&["Profiles", "u", "Buildofficе"]),
            abs(&["Profiles", "u", "Ｂuildoffice"]),
            abs(&["Profiles", "u", "BUİLDOFFİCE"]),
            PathBuf::new(),
        ] {
            assert!(!expected.matches(&bad), "断るべき：{}", bad.display());
            let err = expected.check(&bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidRoot);
            assert!(!err.message().contains("Profiles"), "{}", err.message());
            assert!(!err.message().contains("Buildoffice"), "{}", err.message());
        }
        // ASCII でない名前は、大文字小文字の違いを同じにしない（Unicode の大文字小文字を使わない）
        let expected = ExpectedRoot::new(abs(&["Profiles", "u"]), "ärbeit");
        assert!(expected.matches(&abs(&["Profiles", "u", "ärbeit"])));
        assert!(expected.matches(&abs(&["Profiles", "u", "äRBEIT"])));
        assert!(!expected.matches(&abs(&["Profiles", "u", "Ärbeit"])));
        let expected = ExpectedRoot::new(abs(&["Profiles", "u"]), "keep");
        assert!(expected.matches(&abs(&["Profiles", "u", "KEEP"])));
        // K（U+212A ケルビン）は Unicode では k と同じに扱われることがあるが、ここでは違う名前
        assert!(!expected.matches(&abs(&["Profiles", "u", "\u{212A}eep"])));
    }

    /// Windows：`\\?\` の付かない形と付いた形は、同じ要素の並びでない（どちらもハンドルの最終のパスで比べる）。
    #[cfg(windows)]
    #[test]
    fn expected_root_compares_the_resolved_forms() {
        let expected = ExpectedRoot::new(PathBuf::from(r"\\?\C:\Profiles\u"), "Buildoffice");
        assert!(expected.matches(Path::new(r"\\?\C:\Profiles\u\Buildoffice")));
        assert!(expected.matches(Path::new(r"\\?\C:\Profiles\u\Buildoffice\")));
        assert!(!expected.matches(Path::new(r"C:\Profiles\u\Buildoffice")));
        assert!(!expected.matches(Path::new(r"\\?\D:\Profiles\u\Buildoffice")));
        assert!(!expected.matches(Path::new(r"\\?\UNC\server\share\u\Buildoffice")));
    }

    /// os_home：Windows では、OS に問い合わせたホームが絶対パスで、この PC の USERPROFILE と（大文字小文字を問わずに）
    /// 同じ（調べるだけ。何も作らない）。
    #[cfg(windows)]
    #[test]
    fn os_home_is_the_profile_folder() {
        let home = guard::os_home().expect("ホームを問い合わせられる");
        // 失敗の文にも、本物のホームのパスを出さない
        assert!(home.is_absolute(), "OS のホームが絶対パスでない");
        let variable = std::env::var_os("USERPROFILE").expect("USERPROFILE がある");
        assert!(
            home.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&variable.to_string_lossy()),
            "OS のホームと USERPROFILE が違う"
        );
    }

    /// Windows 以外は、既定の保管庫をまだ作らない（NOT_IMPLEMENTED）。
    #[cfg(not(windows))]
    #[test]
    fn os_home_is_not_implemented_outside_windows() {
        let err = guard::os_home().unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotImplemented);
    }
}
